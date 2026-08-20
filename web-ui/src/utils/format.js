export function formatBytes(n) {
  if (n === null || n === undefined) return '-'
  if (n < 1024) return `${n} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let v = n / 1024
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i++
  }
  return `${v.toFixed(v < 10 ? 2 : 1)} ${units[i]}`
}

export function formatDuration(secs) {
  if (secs === null || secs === undefined || secs < 0) return '从未'
  if (secs < 60) return `${secs}秒前`
  if (secs < 3600) return `${Math.floor(secs / 60)}分钟前`
  if (secs < 86400) return `${Math.floor(secs / 3600)}小时前`
  return `${Math.floor(secs / 86400)}天前`
}
