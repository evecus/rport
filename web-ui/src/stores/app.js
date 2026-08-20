import { defineStore } from 'pinia'
import { api } from '../api'

export const useAppStore = defineStore('app', {
  state: () => ({
    status: null, // { version, mode, running, is_runnable, config_path }
    config: null,
    loading: false,
    pollTimer: null,
  }),
  getters: {
    mode: (state) => state.status?.mode || 'server',
  },
  actions: {
    async refreshStatus() {
      this.status = await api.getStatus()
    },
    async refreshConfig() {
      this.config = await api.getConfig()
    },
    async refreshAll() {
      await Promise.all([this.refreshStatus(), this.refreshConfig()])
    },
    startPolling(intervalMs = 5000) {
      this.stopPolling()
      this.pollTimer = setInterval(() => {
        this.refreshStatus().catch(() => {})
      }, intervalMs)
    },
    stopPolling() {
      if (this.pollTimer) {
        clearInterval(this.pollTimer)
        this.pollTimer = null
      }
    },
  },
})
