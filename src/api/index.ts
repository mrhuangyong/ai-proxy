import { reactive } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { isTauri } from '../utils/env'

const DEFAULT_PROXY_PORT = 7860
const TOKEN_KEY = 'ai_proxy_token'

export const LOGIN_ROUTE = '/login'

function getStoredToken(): string | null {
  if (isTauri) return null
  return localStorage.getItem(TOKEN_KEY)
}

export function clearStoredToken() {
  if (!isTauri) {
    localStorage.removeItem(TOKEN_KEY)
  }
}

let onUnauthorized: (() => void) | null = null

export function setUnauthorizedHandler(handler: () => void) {
  onUnauthorized = handler
}

export class ApiError extends Error {
  status: number
  data: unknown
  constructor(message: string, status: number, data?: unknown) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.data = data
  }
}

export const apiState = reactive({
  baseUrl: '',
  proxyPort: DEFAULT_PROXY_PORT,
  connectHost: '127.0.0.1',
  initialized: false,
})

function normalizeApiConfigUrl(configUrl: string): { baseUrl: string; port: number; host: string } {
  const normalizedInput = /^[a-zA-Z][a-zA-Z\d+\-.]*:\/\//.test(configUrl) ? configUrl : `http://${configUrl}`
  const url = new URL(normalizedInput)
  const host = url.hostname === '0.0.0.0' ? '127.0.0.1' : url.hostname
  const port = parseInt(url.port, 10) || DEFAULT_PROXY_PORT
  const baseUrl = `${url.protocol}//${host}:${port}`
  return { baseUrl, port, host }
}

function buildApiUrl(path: string): string {
  return apiState.baseUrl ? `${apiState.baseUrl}${path}` : path
}

function setApiState(baseUrl: string, port: number, host: string, initialized: boolean): void {
  apiState.baseUrl = baseUrl
  apiState.proxyPort = port
  apiState.connectHost = host
  apiState.initialized = initialized
}

async function tryFetchHealth(url: string): Promise<boolean> {
  try {
    const res = await fetch(url)
    if (!res.ok) {
      return false
    }
    const text = await res.text()
    return text.trim() === 'OK'
  } catch {
    return false
  }
}

async function loadTauriConfig(): Promise<{ baseUrl: string; port: number; host: string } | null> {
  try {
    const configUrl = await invoke<string>('get_api_config')
    return normalizeApiConfigUrl(configUrl)
  } catch {
    return null
  }
}

export async function initApi(force = false): Promise<void> {
  if (apiState.initialized && !force) {
    return
  }

  if (force) {
    setApiState('', apiState.proxyPort || DEFAULT_PROXY_PORT, apiState.connectHost || '127.0.0.1', false)
  }

  // Try relative URL first (works through Vite proxy in dev mode)
  // Use exponential backoff: 1s, 2s, 4s, 8s... to avoid flooding logs during Rust compilation
  for (let i = 0; i < 20; i++) {
    if (await tryFetchHealth('/health')) {
      const config = await loadTauriConfig()
      if (config) {
        setApiState('', config.port, config.host, true)
      } else {
        setApiState('', apiState.proxyPort || DEFAULT_PROXY_PORT, apiState.connectHost || '127.0.0.1', true)
      }
      return
    }

    // Fallback: try absolute URL via Tauri config
    const config = await loadTauriConfig()
    if (config && await tryFetchHealth(buildApiUrlFromBase(config.baseUrl, '/health'))) {
      setApiState(config.baseUrl, config.port, config.host, true)
      return
    }

    // Exponential backoff: 1s, 2s, 4s, 8s, 16s... max 30s
    const delay = Math.min(1000 * Math.pow(2, i), 30000)
    await new Promise(r => setTimeout(r, delay))
  }

  throw new Error('Proxy server not reachable')
}

function buildApiUrlFromBase(baseUrl: string, path: string): string {
  return `${baseUrl}${path}`
}

export async function refreshApiConfig(): Promise<void> {
  await initApi(true)
}

async function ensureInitialized(): Promise<void> {
  if (apiState.initialized) return
  await initApi()
}

export async function api<T>(path: string, options?: RequestInit): Promise<T> {
  await ensureInitialized()

  const requestUrl = buildApiUrl(path)
  const token = getStoredToken()
  const headers: Record<string, string> = { 'Content-Type': 'application/json' }
  if (token && !isTauri) {
    headers['Authorization'] = `Bearer ${token}`
  }
  let res: Response
  try {
    res = await fetch(requestUrl, {
      ...options,
      headers: { ...headers, ...(options?.headers as Record<string, string> | undefined) },
    })
  } catch (error) {
    throw error
  }
  if (res.status === 401 && !isTauri) {
    clearStoredToken()
    if (onUnauthorized) onUnauthorized()
    throw new ApiError('Unauthorized', 401)
  }
  if (!res.ok) {
    const body = await res.json().catch(() => ({ error: `HTTP ${res.status}` }))
    const err = new ApiError(body.error || `API error: ${res.status}`, res.status, body.data)
    throw err
  }
  let body: { success?: boolean; error?: string; data?: T }
  try {
    body = await res.json() as { success?: boolean; error?: string; data?: T }
  } catch (error) {
    throw error
  }
  if (!body.success) throw new ApiError(body.error || 'Unknown error', res.status, body.data)
  return body.data as T
}

export function getBaseUrl(): string {
  return apiState.baseUrl
}

export function getProxyPort(): number {
  return apiState.proxyPort
}

export function isInitialized(): boolean {
  return apiState.initialized
}

// --- Backup & Sync APIs ---

export interface BackupStatus {
  passphrase_set: boolean
}

export interface ExportResult {
  data: string // base64
}

export interface SyncConfigResponse {
  enabled: boolean
  webdav_url: string
  webdav_username: string
  webdav_password?: string
  webdav_path: string
  auto_enabled: boolean
  auto_interval_minutes: number
  sync_on_change: boolean
  retention_count: number
}

export interface RemoteBackup {
  filename: string
  size: number
  modified_at: string
}

export interface SyncLastStatus {
  last_upload_at: string
  last_upload_status: string
  last_error: string
}

export const backupApi = {
  getStatus: () => api<BackupStatus>('/api/backup/status'),
  setPassphrase: (new_passphrase: string, old_passphrase?: string) =>
    api('/api/backup/passphrase', {
      method: 'PUT',
      body: JSON.stringify({ new_passphrase, old_passphrase }),
    }),
  exportBackup: () => api<ExportResult>('/api/backup/export', { method: 'POST' }),
  importBackup: (file_bytes: string, passphrase?: string) =>
    api('/api/backup/import', {
      method: 'POST',
      body: JSON.stringify({ file_bytes, passphrase }),
    }),
}

export const syncApi = {
  getConfig: () => api<SyncConfigResponse>('/api/sync/config'),
  saveConfig: (cfg: Partial<{
    enabled: boolean
    webdav_url: string
    webdav_username: string
    webdav_password: string
    webdav_path: string
    auto_enabled: boolean
    auto_interval_minutes: number
    sync_on_change: boolean
    retention_count: number
  }>) =>
    api('/api/sync/config', { method: 'PUT', body: JSON.stringify(cfg) }),
  testConnection: () => api<{ success: boolean; error?: string }>('/api/sync/test', { method: 'POST' }),
  prune: () => api<{ removed: number; remaining: number }>('/api/sync/prune', { method: 'POST' }),
  upload: () => api<{ filename: string; size: number }>('/api/sync/upload', { method: 'POST' }),
  listVersions: () => api<RemoteBackup[]>('/api/sync/versions'),
  restore: (filename: string, passphrase?: string) =>
    api('/api/sync/restore', { method: 'POST', body: JSON.stringify({ filename, passphrase }) }),
  deleteVersion: (filename: string) =>
    api(`/api/sync/versions/${encodeURIComponent(filename)}`, { method: 'DELETE' }),
  getLast: () => api<SyncLastStatus>('/api/sync/last'),
}
