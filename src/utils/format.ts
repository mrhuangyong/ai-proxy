export function formatDateTime(
  value: string | number | Date,
  opts?: { utc?: boolean },
): string {
  let raw = typeof value === 'string' ? value : ''
  if (
    opts?.utc &&
    raw &&
    !/[zZ]$/.test(raw.trim()) &&
    !/[+-]\d{2}:?\d{2}$/.test(raw.trim())
  ) {
    raw = raw.trim().replace(' ', 'T') + 'Z'
  }
  const d = new Date(raw)
  if (isNaN(d.getTime())) return typeof value === 'string' ? value : ''
  const p = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
}
