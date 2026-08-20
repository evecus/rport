import { defineStore } from 'pinia'
import { api } from '../api'

export const useAuthStore = defineStore('auth', {
  state: () => ({
    // 登录态无法从 HttpOnly cookie 读出，靠“调用过一次需要鉴权的接口且成功”来判断
    checked: false,
    loggedIn: false,
  }),
  actions: {
    async login(username, password) {
      await api.login(username, password)
      this.loggedIn = true
      this.checked = true
    },
    async logout() {
      try {
        await api.logout()
      } finally {
        this.loggedIn = false
      }
    },
    async checkAuth() {
      try {
        await api.getStatus()
        this.loggedIn = true
      } catch (e) {
        this.loggedIn = false
      } finally {
        this.checked = true
      }
      return this.loggedIn
    },
  },
})
