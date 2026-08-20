<script setup>
import { ref, onMounted, onUnmounted, computed } from 'vue'
import { useRouter } from 'vue-router'
import { useAppStore } from '../stores/app'
import { api } from '../api'
import StatCard from '../components/StatCard.vue'
import SessionList from '../components/SessionList.vue'
import ClientProxyList from '../components/ClientProxyList.vue'
import { formatBytes } from '../utils/format'

const router = useRouter()
const appStore = useAppStore()

const sessions = ref([])
const clientProxies = ref([])
const loading = ref(true)
let timer = null

async function refreshData() {
  try {
    if (appStore.mode === 'server') {
      sessions.value = await api.getSessions()
    } else {
      clientProxies.value = await api.getClientMetrics()
    }
  } catch (e) {
    // 轮询请求失败先静默，状态栏会体现异常
  } finally {
    loading.value = false
  }
}

onMounted(async () => {
  await appStore.refreshAll()
  await refreshData()
  timer = setInterval(refreshData, 3000)
})
onUnmounted(() => timer && clearInterval(timer))

const isRunnable = computed(() => appStore.status?.is_runnable)
const running = computed(() => appStore.status?.running)

const onlineCount = computed(() => sessions.value.filter((s) => s.connected).length)
const totalProxies = computed(() =>
  appStore.mode === 'server'
    ? sessions.value.reduce((sum, s) => sum + s.proxies.length, 0)
    : clientProxies.value.length
)
const totalUp = computed(() => {
  const list = appStore.mode === 'server'
    ? sessions.value.flatMap((s) => s.proxies)
    : clientProxies.value
  return list.reduce((sum, p) => sum + (p.bytes_up || 0), 0)
})
const totalDown = computed(() => {
  const list = appStore.mode === 'server'
    ? sessions.value.flatMap((s) => s.proxies)
    : clientProxies.value
  return list.reduce((sum, p) => sum + (p.bytes_down || 0), 0)
})
</script>

<template>
  <div class="dashboard">
    <div class="page-head">
      <div>
        <h1>概览</h1>
        <p class="sub">
          {{ appStore.mode === 'server' ? '服务端运行状态与客户端连接情况' : '客户端连接状态与代理流量' }}
        </p>
      </div>
      <span
        class="badge"
        :class="running ? 'badge-success' : (isRunnable ? 'badge-warning' : 'badge-danger')"
      >
        <span class="dot" :class="running ? 'dot-success' : (isRunnable ? 'dot-warning' : 'dot-danger')"></span>
        {{ running ? '运行中' : (isRunnable ? '已停止' : '待配置') }}
      </span>
    </div>

    <div v-if="!isRunnable" class="setup-banner card">
      <div>
        <div class="setup-title">配置尚未完成</div>
        <div class="setup-sub">需要先完成 {{ appStore.mode === 'server' ? '服务端' : '客户端' }} 的基础配置才能启动</div>
      </div>
      <button class="btn btn-primary" @click="router.push({ name: 'config' })">前往配置</button>
    </div>

    <template v-else>
      <div class="stats-grid">
        <StatCard
          v-if="appStore.mode === 'server'"
          label="在线客户端"
          :value="`${onlineCount} / ${sessions.length}`"
        />
        <StatCard label="代理数量" :value="totalProxies" />
        <StatCard label="累计上行" :value="formatBytes(totalUp)" />
        <StatCard label="累计下行" :value="formatBytes(totalDown)" />
      </div>

      <section class="section">
        <h2>{{ appStore.mode === 'server' ? '客户端连接' : '代理流量' }}</h2>
        <SessionList v-if="appStore.mode === 'server'" :sessions="sessions" :loading="loading" />
        <ClientProxyList v-else :proxies="clientProxies" :loading="loading" />
      </section>
    </template>
  </div>
</template>

<style scoped>
.page-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  margin-bottom: 28px;
}
h1 {
  font-size: 22px;
  margin: 0 0 4px;
  letter-spacing: -0.4px;
}
.sub {
  color: var(--color-text-muted);
  font-size: 13.5px;
  margin: 0;
}

.setup-banner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 22px 26px;
  margin-bottom: 24px;
  background: linear-gradient(135deg, #fffbeb 0%, #fff7ed 100%);
  border-color: #fde68a;
}
.setup-title {
  font-weight: 700;
  font-size: 15px;
  margin-bottom: 3px;
}
.setup-sub {
  font-size: 13px;
  color: var(--color-text-muted);
}

.stats-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
  gap: 16px;
  margin-bottom: 28px;
}

.section h2 {
  font-size: 15px;
  font-weight: 700;
  margin: 0 0 14px;
}
</style>
