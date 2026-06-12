<template>
  <n-space vertical size="large">
    <n-card title="通用设置">
      <n-form label-placement="left" label-width="140" style="max-width: 520px">
        <n-form-item label="外观主题">
          <n-radio-group v-model:value="themeMode" @update:value="handleThemeChange" size="small">
            <n-radio-button value="light">浅色</n-radio-button>
            <n-radio-button value="dark">深色</n-radio-button>
            <n-radio-button value="system">跟随系统</n-radio-button>
          </n-radio-group>
        </n-form-item>
        <n-form-item v-if="isTauri" label="开机启动">
          <n-switch v-model:value="autostartEnabled" @update:value="handleAutostartChange" />
        </n-form-item>
        <n-form-item>
          <template #label>
            <n-tooltip trigger="hover">
              <template #trigger>
                <span>提取 System 消息</span>
              </template>
              将 messages 数组中的 system/developer 角色消息提取到顶层 system 字段，修复 Claude Code 兼容性
            </n-tooltip>
          </template>
          <n-switch v-model:value="settings.extractSystemFromMessages" />
        </n-form-item>
        <n-form-item label="记录请求体">
          <n-switch v-model:value="settings.recordRequestBody" />
        </n-form-item>
      </n-form>
    </n-card>

    <n-card title="网络设置">
      <n-form label-placement="left" label-width="140" style="max-width: 520px">
        <n-form-item v-if="isTauri" label="HTTP 端口">
          <n-input-number
            v-model:value="settings.port"
            :min="1"
            :max="65535"
            style="width: 100%"
          />
        </n-form-item>
        <n-form-item label="请求超时（秒）">
          <n-input-number
            v-model:value="settings.requestTimeout"
            :min="10"
            :max="3600"
            style="width: 100%"
          />
        </n-form-item>
        <n-form-item label="连接超时（秒）">
          <n-input-number
            v-model:value="settings.connectTimeout"
            :min="1"
            :max="300"
            style="width: 100%"
          />
        </n-form-item>
        <n-form-item label="自动重试次数">
          <n-input-number
            v-model:value="settings.upstreamMaxRetries"
            :min="0"
            :max="50"
            style="width: 100%"
          />
        </n-form-item>
        <n-form-item label="重试基准间隔（ms）">
          <n-input-number
            v-model:value="settings.upstreamRetryBackoffBaseMs"
            :min="0"
            :max="10000"
            style="width: 100%"
          />
        </n-form-item>
        <n-form-item label="日志保留天数">
          <n-input-number
            v-model:value="settings.logRetentionDays"
            :min="1"
            :max="365"
            style="width: 100%"
          />
        </n-form-item>
        <n-form-item label="代理 API Key">
          <n-input
            v-model:value="settings.proxyAuthKey"
            type="password"
            show-password-on="click"
            placeholder="设置 Agent 访问代理时使用的 API Key"
          />
        </n-form-item>
        <n-form-item>
          <n-button type="primary" @click="handleSave">
            保存设置
          </n-button>
        </n-form-item>
      </n-form>
    </n-card>

    <n-card v-if="isTauri" title="检查更新">
      <n-form label-placement="left" label-width="140" style="max-width: 520px">
        <n-form-item label="当前版本">
          <n-text>{{ currentVersion }}</n-text>
        </n-form-item>
        <n-form-item>
          <n-button
            type="primary"
            :loading="checkingUpdate"
            @click="handleCheckUpdate"
          >
            检查更新
          </n-button>
        </n-form-item>
      </n-form>
    </n-card>

    <n-card v-if="isTauri" class="danger-zone">
      <template #header>
        <n-space align="center">
          <n-text strong style="color: var(--error)">危险操作</n-text>
          <n-tag type="error" size="small">谨慎操作</n-tag>
        </n-space>
      </template>
      <n-space vertical>
        <n-text>清除所有数据将删除所有供应商配置、API Key、请求日志等，应用将自动重启。</n-text>
        <n-button type="error" @click="handleResetAll">
          清除所有数据
        </n-button>
      </n-space>
    </n-card>
    <UpdateNotification ref="updateNotification" />
  </n-space>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useMessage } from 'naive-ui'
import { invoke } from '@tauri-apps/api/core'
import { isEnabled, enable, disable } from '@tauri-apps/plugin-autostart'
import { getVersion } from '@tauri-apps/api/app'
import { isTauri } from '../utils/env'
import { api, refreshApiConfig } from '../api'
import { useTheme } from '../theme/use-theme'
import type { ThemeMode } from '../theme/use-theme'
import UpdateNotification from '../components/UpdateNotification.vue'

const { mode: currentThemeMode, setMode } = useTheme()
const message = useMessage()
const themeMode = ref<ThemeMode>(currentThemeMode.value)

interface AppSettings {
  port: number
  requestTimeout: number
  connectTimeout: number
  logRetentionDays: number
  recordRequestBody: boolean
  proxyAuthKey: string
  upstreamMaxRetries: number
  upstreamRetryBackoffBaseMs: number
  extractSystemFromMessages: boolean
}

const settings = ref<AppSettings>({
  port: 7860,
  requestTimeout: 1200,
  connectTimeout: 30,
  logRetentionDays: 30,
  recordRequestBody: false,
  proxyAuthKey: '',
  upstreamMaxRetries: 10,
  upstreamRetryBackoffBaseMs: 500,
  extractSystemFromMessages: true,
})

const savedNetworkConfig = ref({
  port: settings.value.port,
})

const autostartEnabled = ref(false)
const currentVersion = ref('...')
const checkingUpdate = ref(false)
const updateNotification = ref<InstanceType<typeof UpdateNotification> | null>(null)

function handleThemeChange(val: ThemeMode) {
  setMode(val)
}

async function loadSettings() {
  try {
    const data = await api<{
      http_port: string
      request_timeout: string
      connect_timeout: string
      log_retention_days: string
      record_request_body: string
      proxy_auth_enabled: string
      proxy_auth_key: string
    }>('/api/settings')
    settings.value = {
      port: parseInt(data.http_port) || 7860,
      requestTimeout: parseInt(data.request_timeout) || 1200,
      connectTimeout: parseInt(data.connect_timeout) || 30,
      logRetentionDays: parseInt(data.log_retention_days) || 30,
      recordRequestBody: data.record_request_body === 'true',
      proxyAuthKey: data.proxy_auth_key,
      upstreamMaxRetries: parseInt((data as any).upstream_max_retries) || 10,
      upstreamRetryBackoffBaseMs: parseInt((data as any).upstream_retry_backoff_base_ms) || 500,
      extractSystemFromMessages: (data as any).extract_system_from_messages !== 'false',
    }
    savedNetworkConfig.value = {
      port: settings.value.port,
    }
  } catch (error) {
    console.error('Failed to load settings:', error)
  }
}

async function handleSave() {
  const previousPort = savedNetworkConfig.value.port

  if (!settings.value.proxyAuthKey) {
    message.warning('请设置代理 API Key')
    return
  }
  try {
    await api('/api/settings', {
      method: 'PUT',
      body: JSON.stringify({
        http_port: String(settings.value.port),
        request_timeout: String(settings.value.requestTimeout),
        connect_timeout: String(settings.value.connectTimeout),
        log_retention_days: String(settings.value.logRetentionDays),
        record_request_body: String(settings.value.recordRequestBody),
        proxy_auth_enabled: 'true',
        proxy_auth_key: settings.value.proxyAuthKey,
        upstream_max_retries: String(settings.value.upstreamMaxRetries),
        upstream_retry_backoff_base_ms: String(settings.value.upstreamRetryBackoffBaseMs),
        extract_system_from_messages: String(settings.value.extractSystemFromMessages),
      }),
    })

    const portChanged = settings.value.port !== previousPort

    if (isTauri && portChanged) {
      await invoke<string>('apply_proxy_config')
      await refreshApiConfig()
    }

    savedNetworkConfig.value = {
      port: settings.value.port,
    }
    message.success('设置已保存')
  } catch (error) {
    message.error(`保存失败: ${error}`)
  }
}

async function handleAutostartChange(enabled: boolean) {
  try {
    if (enabled) {
      await enable()
    } else {
      await disable()
    }
    message.success(enabled ? '已启用开机启动' : '已关闭开机启动')
  } catch (error) {
    autostartEnabled.value = !enabled
    message.error(`设置失败: ${error}`)
  }
}

async function handleCheckUpdate() {
  checkingUpdate.value = true
  try {
    const result = await invoke<{
      version: string
      release_notes: string
      download_url: string
      published_at: string
    } | null>('check_for_update')
    if (result) {
      updateNotification.value?.show(result)
    } else {
      message.success('已是最新版本')
    }
  } catch (error) {
    message.error(`检查更新失败: ${error}`)
  } finally {
    checkingUpdate.value = false
  }
}


async function handleResetAll() {
  const dialog = window.confirm(
    `⚠️ 确定要清除所有数据吗？

此操作将删除：
- 所有供应商配置和 API Key
- 所有请求日志
- 所有应用配置

应用将自动重启，此操作不可恢复！`
  )
  if (!dialog) return

  const confirm2 = window.confirm(
    '⚠️ 最后确认：所有数据将被永久删除，确定继续吗？'
  )
  if (!confirm2) return

  try {
    await invoke('reset_all_data')
  } catch (error) {
    message.error(`重置失败: ${error}`)
  }
}

onMounted(async () => {
  await loadSettings()
  try {
    autostartEnabled.value = await isEnabled()
  } catch {
    autostartEnabled.value = false
  }
  if (isTauri) {
    try {
      currentVersion.value = await getVersion()
    } catch {
      currentVersion.value = 'unknown'
    }
  }
})
</script>
