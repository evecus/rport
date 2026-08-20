<script setup>
import { ref, reactive, onMounted, computed } from 'vue'
import { useAppStore } from '../stores/app'
import { api } from '../api'

const appStore = useAppStore()

const loading = ref(true)
const saving = ref(false)
const saveError = ref('')
const saveOk = ref(false)
const newPassword = ref('')

// 本地可编辑副本
const form = reactive({
  mode: 'server',
  server: null,
  client: null,
  web: { listen: '0.0.0.0:1080', username: 'admin' },
})

function defaultServer() {
  return {
    bind_addr: '0.0.0.0:7000',
    transport: 'quic',
    token: '',
    proxy_port_range: [20000, 30000],
    log_level: 'info',
    tls: { mode: 'self_signed', sni: 'www.bing.com' },
  }
}
function defaultClient() {
  return {
    server_addr: '',
    transport: 'quic',
    token: '',
    tls_skip_verify: true,
    heartbeat_interval_secs: 10,
    reconnect_max_secs: 30,
    log_level: 'info',
    proxies: [],
  }
}

onMounted(async () => {
  try {
    const cfg = await api.getConfig()
    form.mode = cfg.mode || 'server'
    form.server = cfg.server || defaultServer()
    if (form.server && !Array.isArray(form.server.proxy_port_range)) {
      form.server.proxy_port_range = [20000, 30000]
    }
    form.client = cfg.client || defaultClient()
    form.web = cfg.web || form.web
  } catch (e) {
    // 保留默认表单
  } finally {
    loading.value = false
  }
})

function switchMode(mode) {
  form.mode = mode
  if (mode === 'server' && !form.server) form.server = defaultServer()
  if (mode === 'client' && !form.client) form.client = defaultClient()
}

function addProxy() {
  form.client.proxies.push({
    name: `proxy-${form.client.proxies.length + 1}`,
    type: 'tcp',
    local_addr: '127.0.0.1:8080',
    remote_port: 8080,
  })
}
function removeProxy(idx) {
  form.client.proxies.splice(idx, 1)
}

async function handleSave() {
  saving.value = true
  saveError.value = ''
  saveOk.value = false
  try {
    const payload = {
      mode: form.mode,
      server: form.mode === 'server' ? form.server : null,
      client: form.mode === 'client' ? form.client : null,
      web: form.web,
    }
    await api.saveConfig(payload, newPassword.value || undefined)
    saveOk.value = true
    newPassword.value = ''
    await appStore.refreshStatus()
    setTimeout(() => (saveOk.value = false), 3000)
  } catch (e) {
    saveError.value = e.message || '保存失败'
  } finally {
    saving.value = false
  }
}

const transportOptions = ['quic', 'tcp', 'websocket', 'xhttp']
</script>

<template>
  <div class="config-editor">
    <div class="page-head">
      <div>
        <h1>配置</h1>
        <p class="sub">修改后保存会立即热更新生效，无需重启进程</p>
      </div>
      <button class="btn btn-primary" @click="handleSave" :disabled="saving || loading">
        {{ saving ? '保存中…' : '保存并应用' }}
      </button>
    </div>

    <transition name="fade">
      <div v-if="saveOk" class="toast toast-success">配置已保存并生效</div>
    </transition>
    <transition name="fade">
      <div v-if="saveError" class="toast toast-error">{{ saveError }}</div>
    </transition>

    <div v-if="loading" class="empty">加载中…</div>

    <template v-else>
      <!-- 模式切换 -->
      <section class="card section-card">
        <h2>运行模式</h2>
        <div class="mode-switch">
          <button
            class="mode-btn"
            :class="{ active: form.mode === 'server' }"
            @click="switchMode('server')"
          >
            服务端
            <span class="mode-desc">对外提供穿透入口</span>
          </button>
          <button
            class="mode-btn"
            :class="{ active: form.mode === 'client' }"
            @click="switchMode('client')"
          >
            客户端
            <span class="mode-desc">连接到服务端，暴露本地服务</span>
          </button>
        </div>
      </section>

      <!-- 服务端配置 -->
      <section v-if="form.mode === 'server' && form.server" class="card section-card">
        <h2>服务端配置</h2>
        <div class="grid-2">
          <div class="field">
            <label class="label">监听地址</label>
            <input class="input" v-model="form.server.bind_addr" placeholder="0.0.0.0:7000" />
          </div>
          <div class="field">
            <label class="label">传输模式</label>
            <select class="input" v-model="form.server.transport">
              <option v-for="t in transportOptions" :key="t" :value="t">{{ t }}</option>
            </select>
          </div>
          <div class="field">
            <label class="label">认证 Token</label>
            <input class="input" v-model="form.server.token" placeholder="客户端连接口令" />
          </div>
          <div class="field">
            <label class="label">代理端口范围</label>
            <div class="port-range">
              <input class="input" type="number" v-model.number="form.server.proxy_port_range[0]" placeholder="起始端口" />
              <span class="range-sep">–</span>
              <input class="input" type="number" v-model.number="form.server.proxy_port_range[1]" placeholder="结束端口" />
            </div>
          </div>
          <div class="field">
            <label class="label">日志级别</label>
            <input class="input" v-model="form.server.log_level" placeholder="info" />
          </div>
        </div>

        <div class="tls-section">
          <label class="label">TLS 模式</label>
          <div class="mode-switch tls-switch">
            <button
              class="mode-btn small"
              :class="{ active: form.server.tls.mode === 'self_signed' }"
              @click="form.server.tls = { mode: 'self_signed', sni: form.server.tls.sni || 'www.bing.com' }"
            >自签名</button>
            <button
              class="mode-btn small"
              :class="{ active: form.server.tls.mode === 'manual' }"
              @click="form.server.tls = { mode: 'manual', cert_file: form.server.tls.cert_file || '', key_file: form.server.tls.key_file || '' }"
            >手动证书</button>
            <button
              class="mode-btn small"
              :class="{ active: form.server.tls.mode === 'acme' }"
              @click="form.server.tls = { mode: 'acme', domain: form.server.tls.domain || '', email: form.server.tls.email || '', cf_api_token: form.server.tls.cf_api_token || '', staging: false }"
            >ACME 自动申请</button>
          </div>

          <div v-if="form.server.tls.mode === 'self_signed'" class="grid-2 tls-fields">
            <div class="field">
              <label class="label">伪装 SNI</label>
              <input class="input" v-model="form.server.tls.sni" placeholder="www.bing.com" />
            </div>
          </div>

          <div v-else-if="form.server.tls.mode === 'manual'" class="grid-2 tls-fields">
            <div class="field">
              <label class="label">证书文件路径</label>
              <input class="input" v-model="form.server.tls.cert_file" placeholder="/path/to/cert.pem" />
            </div>
            <div class="field">
              <label class="label">私钥文件路径</label>
              <input class="input" v-model="form.server.tls.key_file" placeholder="/path/to/key.pem" />
            </div>
          </div>

          <div v-else-if="form.server.tls.mode === 'acme'" class="grid-2 tls-fields">
            <div class="field">
              <label class="label">域名</label>
              <input class="input" v-model="form.server.tls.domain" placeholder="example.com" />
            </div>
            <div class="field">
              <label class="label">账号邮箱</label>
              <input class="input" v-model="form.server.tls.email" placeholder="you@example.com" />
            </div>
            <div class="field">
              <label class="label">Cloudflare API Token</label>
              <input class="input" type="password" v-model="form.server.tls.cf_api_token" />
            </div>
            <div class="field checkbox-field">
              <label class="checkbox-label">
                <input type="checkbox" v-model="form.server.tls.staging" />
                使用 staging 环境（测试用）
              </label>
            </div>
          </div>
        </div>
      </section>

      <!-- 客户端配置 -->
      <template v-if="form.mode === 'client' && form.client">
        <section class="card section-card">
          <h2>客户端配置</h2>
          <div class="grid-2">
            <div class="field">
              <label class="label">服务端地址</label>
              <input class="input" v-model="form.client.server_addr" placeholder="example.com:7000" />
            </div>
            <div class="field">
              <label class="label">传输模式</label>
              <select class="input" v-model="form.client.transport">
                <option v-for="t in transportOptions" :key="t" :value="t">{{ t }}</option>
              </select>
            </div>
            <div class="field">
              <label class="label">认证 Token</label>
              <input class="input" v-model="form.client.token" placeholder="需与服务端一致" />
            </div>
            <div class="field">
              <label class="label">心跳间隔（秒）</label>
              <input class="input" type="number" v-model.number="form.client.heartbeat_interval_secs" />
            </div>
            <div class="field checkbox-field">
              <label class="checkbox-label">
                <input type="checkbox" v-model="form.client.tls_skip_verify" />
                跳过服务端证书校验（自签名证书时勾选）
              </label>
            </div>
          </div>
        </section>

        <section class="card section-card">
          <div class="proxy-head">
            <h2>代理列表</h2>
            <button class="btn btn-ghost" @click="addProxy">+ 添加代理</button>
          </div>

          <div v-if="form.client.proxies.length === 0" class="empty">
            还没有代理，点击"添加代理"开始
          </div>
          <div v-else class="proxy-edit-list">
            <div v-for="(p, idx) in form.client.proxies" :key="idx" class="proxy-edit-row">
              <input class="input" v-model="p.name" placeholder="代理名称" />
              <select class="input" v-model="p.type">
                <option value="tcp">TCP</option>
                <option value="udp">UDP</option>
              </select>
              <input class="input" v-model="p.local_addr" placeholder="127.0.0.1:8080" />
              <input class="input" type="number" v-model.number="p.remote_port" placeholder="远程端口" />
              <button class="btn btn-danger remove-btn" @click="removeProxy(idx)">删除</button>
            </div>
          </div>
        </section>
      </template>

      <!-- Web UI 凭据 -->
      <section class="card section-card">
        <h2>Web 管理界面</h2>
        <div class="grid-2">
          <div class="field">
            <label class="label">监听地址</label>
            <input class="input" v-model="form.web.listen" placeholder="0.0.0.0:1080" />
          </div>
          <div class="field">
            <label class="label">用户名</label>
            <input class="input" v-model="form.web.username" />
          </div>
          <div class="field">
            <label class="label">修改密码（留空则不变）</label>
            <input class="input" type="password" v-model="newPassword" placeholder="新密码" />
          </div>
        </div>
      </section>
    </template>
  </div>
</template>

<style scoped>
.page-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  margin-bottom: 24px;
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

.toast {
  padding: 10px 16px;
  border-radius: 10px;
  font-size: 13px;
  font-weight: 600;
  margin-bottom: 16px;
}
.toast-success { background: var(--color-success-soft); color: var(--color-success); }
.toast-error { background: var(--color-danger-soft); color: var(--color-danger); }

.empty {
  padding: 40px 0;
  text-align: center;
  color: var(--color-text-faint);
  font-size: 13.5px;
}

.section-card {
  padding: 22px 26px;
  margin-bottom: 20px;
}
.section-card h2 {
  font-size: 15px;
  font-weight: 700;
  margin: 0 0 16px;
}

.mode-switch {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
}
.mode-btn {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 4px;
  padding: 16px 18px;
  border-radius: 12px;
  border: 1.5px solid var(--color-border);
  background: var(--color-surface);
  font-weight: 700;
  font-size: 14.5px;
  color: var(--color-text-muted);
  text-align: left;
  transition: all 0.15s var(--ease);
}
.mode-btn:hover {
  border-color: var(--color-primary);
}
.mode-btn.active {
  border-color: var(--color-primary);
  background: var(--color-primary-soft);
  color: var(--color-primary);
}
.mode-desc {
  font-size: 12px;
  font-weight: 500;
  color: var(--color-text-faint);
}
.mode-btn.active .mode-desc {
  color: var(--color-primary);
  opacity: 0.75;
}

.grid-2 {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 16px 20px;
}

.tls-section {
  margin-top: 20px;
  padding-top: 20px;
  border-top: 1px solid var(--color-border);
}
.tls-switch {
  grid-template-columns: repeat(3, 1fr);
  margin-bottom: 16px;
}
.mode-btn.small {
  padding: 10px 14px;
  font-size: 13px;
}
.tls-fields {
  padding-top: 4px;
}

.field {
  min-width: 0;
}

.checkbox-field {
  display: flex;
  align-items: center;
}
.checkbox-label {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13.5px;
  font-weight: 600;
  color: var(--color-text-muted);
  cursor: pointer;
}
.checkbox-label input {
  width: 16px;
  height: 16px;
}

.port-range {
  display: flex;
  align-items: center;
  gap: 8px;
}
.range-sep {
  color: var(--color-text-faint);
  font-weight: 600;
}

.proxy-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 16px;
}
.proxy-head h2 { margin: 0; }

.proxy-edit-list {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.proxy-edit-row {
  display: grid;
  grid-template-columns: 1.2fr 0.8fr 1.4fr 0.9fr auto;
  gap: 10px;
  align-items: center;
}
.remove-btn {
  padding: 9px 14px;
}

@media (max-width: 720px) {
  .grid-2, .mode-switch { grid-template-columns: 1fr; }
  .proxy-edit-row { grid-template-columns: 1fr; }
}
</style>
