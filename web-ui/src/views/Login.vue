<script setup>
import { ref } from 'vue'
import { useRouter, useRoute } from 'vue-router'
import { useAuthStore } from '../stores/auth'

const router = useRouter()
const route = useRoute()
const auth = useAuthStore()

const username = ref('admin')
const password = ref('')
const loading = ref(false)
const error = ref('')

async function handleSubmit() {
  if (!username.value || !password.value) return
  loading.value = true
  error.value = ''
  try {
    await auth.login(username.value, password.value)
    const redirect = route.query.redirect || '/'
    router.push(redirect)
  } catch (e) {
    error.value = e.status === 401 ? '用户名或密码错误' : '登录失败，请稍后重试'
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <div class="login-page">
    <div class="login-card card">
      <div class="mark">tx</div>
      <h1>登录 tunx</h1>
      <p class="sub">管理配置、查看连接与流量情况</p>

      <form @submit.prevent="handleSubmit">
        <div class="field">
          <label class="label">用户名</label>
          <input class="input" v-model="username" autocomplete="username" />
        </div>
        <div class="field">
          <label class="label">密码</label>
          <input
            class="input"
            type="password"
            v-model="password"
            autocomplete="current-password"
            placeholder="首次启动的密码见启动日志"
          />
        </div>

        <transition name="fade">
          <div v-if="error" class="error-box">{{ error }}</div>
        </transition>

        <button class="btn btn-primary submit-btn" type="submit" :disabled="loading">
          {{ loading ? '登录中…' : '登录' }}
        </button>
      </form>

      <p class="hint">
        忘记密码？删除配置文件中 <code>[web] password_hash</code> 字段后重启即可重新生成。
      </p>
    </div>
  </div>
</template>

<style scoped>
.login-page {
  min-height: 100vh;
  display: flex;
  align-items: center;
  justify-content: center;
  background:
    radial-gradient(circle at 15% 20%, rgba(99, 102, 241, 0.08), transparent 40%),
    radial-gradient(circle at 85% 80%, rgba(139, 92, 246, 0.08), transparent 40%),
    var(--color-bg);
}

.login-card {
  width: 380px;
  padding: 40px 36px;
  text-align: center;
}

.mark {
  width: 52px;
  height: 52px;
  margin: 0 auto 20px;
  border-radius: 14px;
  background: var(--gradient-primary);
  color: white;
  font-weight: 800;
  font-size: 18px;
  display: flex;
  align-items: center;
  justify-content: center;
  letter-spacing: -0.5px;
  box-shadow: 0 8px 20px rgba(99, 102, 241, 0.35);
}

h1 {
  font-size: 20px;
  margin: 0 0 6px;
  letter-spacing: -0.3px;
}

.sub {
  color: var(--color-text-muted);
  font-size: 13px;
  margin: 0 0 28px;
}

.field {
  text-align: left;
  margin-bottom: 16px;
}

.error-box {
  background: var(--color-danger-soft);
  color: var(--color-danger);
  font-size: 13px;
  font-weight: 600;
  padding: 10px 12px;
  border-radius: 10px;
  margin-bottom: 16px;
  text-align: left;
}

.submit-btn {
  width: 100%;
  padding: 11px;
  font-size: 14.5px;
}

.hint {
  margin-top: 22px;
  font-size: 12px;
  color: var(--color-text-faint);
  line-height: 1.6;
}
.hint code {
  background: var(--color-bg);
  padding: 1px 5px;
  border-radius: 4px;
  font-size: 11.5px;
}
</style>
