const BASE = '/api'

class ApiError extends Error {
  constructor(status, message) {
    super(message)
    this.status = status
  }
}

async function request(path, options = {}) {
  const resp = await fetch(BASE + path, {
    credentials: 'include',
    headers: {
      'Content-Type': 'application/json',
      ...(options.headers || {}),
    },
    ...options,
  })

  if (resp.status === 401) {
    throw new ApiError(401, 'unauthorized')
  }

  const contentType = resp.headers.get('content-type') || ''
  const data = contentType.includes('application/json') ? await resp.json() : null

  if (!resp.ok) {
    throw new ApiError(resp.status, (data && data.message) || `request failed: ${resp.status}`)
  }
  return data
}

export const api = {
  login(username, password) {
    return request('/auth/login', {
      method: 'POST',
      body: JSON.stringify({ username, password }),
    })
  },
  logout() {
    return request('/auth/logout', { method: 'POST' })
  },
  getStatus() {
    return request('/status')
  },
  getConfig() {
    return request('/config')
  },
  saveConfig(config, newPassword) {
    const body = { ...config }
    if (newPassword) body.new_password = newPassword
    return request('/config', {
      method: 'PUT',
      body: JSON.stringify(body),
    })
  },
  getSessions() {
    return request('/sessions')
  },
  getClientMetrics() {
    return request('/client-metrics')
  },
}

export { ApiError }
