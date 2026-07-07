<template>
  <n-tabs type="line" animated size="large">
    <!-- 通用 -->
    <n-tab-pane name="general" tab="通用">
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
        <n-form-item label="中断重试模式">
          <n-radio-group v-model:value="settings.upstreamInvisibleRetryMode">
            <n-space>
              <n-radio value="pre_first_token">首字节前（保留流式）</n-radio>
              <n-radio value="full_buffer">完全缓冲（最稳，TTFT 长）</n-radio>
            </n-space>
          </n-radio-group>
        </n-form-item>
        <n-form-item label="重试总超时（秒）">
          <n-input-number v-model:value="settings.upstreamInvisibleRetryTotalTimeoutSecs" :min="30" :max="3600" style="width: 100%" />
        </n-form-item>
        <n-form-item v-if="settings.upstreamInvisibleRetryMode === 'full_buffer'" label="缓冲上限（MB，仅完全缓冲模式）">
          <n-input-number v-model:value="settings.upstreamInvisibleRetryBufferLimitMb" :min="1" :max="256" style="width: 100%" />
        </n-form-item>
        <n-form-item label="日志保留天数">
          <n-input-number
            v-model:value="settings.logRetentionDays"
            :min="1"
            :max="365"
            style="width: 100%"
          />
        </n-form-item>
        <n-form-item>
          <template #label>
            <n-tooltip trigger="hover">
              <template #trigger>
                <span>上游 User-Agent</span>
              </template>
              部分模型计划（coding/token/agent plan）会按 UA 限制客户端。
              配置后转发上游时将使用此 UA；留空则透传客户端 UA。
            </n-tooltip>
          </template>
          <n-input-group>
            <n-input
              v-model:value="settings.upstreamUserAgent"
              placeholder="留空则透传客户端 UA"
              :input-props="{ autocapitalize: 'off' }"
            />
            <n-button @click="settings.upstreamUserAgent = CLAUDE_CLI_UA">
              Claude CLI
            </n-button>
          </n-input-group>
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
      </n-space>
    </n-tab-pane>

    <!-- 数据与同步 -->
    <n-tab-pane name="data" tab="数据与同步">
      <n-space vertical size="large">
    <!-- 备份与恢复 -->
    <n-card title="备份与恢复">
      <n-space vertical>
        <n-space align="center">
          <n-tag :type="passphraseSet ? 'success' : 'warning'">
            口令: {{ passphraseSet ? '已设置' : '未设置' }}
          </n-tag>
          <n-button size="small" @click="passphraseForm.old=passphraseForm.new1=passphraseForm.new2=''; showPassphraseModal = true">
            {{ passphraseSet ? '修改口令' : '设置口令' }}
          </n-button>
        </n-space>
        <n-divider />
        <n-space>
          <n-button type="primary" :disabled="!passphraseSet" @click="exportBackup">导出备份</n-button>
          <n-button @click="pickImportFile">导入恢复</n-button>
        </n-space>
      </n-space>
    </n-card>

    <!-- 远程同步 -->
    <n-card title="远程同步 (WebDAV)">
      <n-space vertical>
        <n-checkbox v-model:checked="syncCfg.enabled">启用同步</n-checkbox>

        <n-form label-placement="left" :label-width="90">
          <n-form-item label="服务器地址">
            <n-input v-model:value="syncCfg.webdav_url" placeholder="https://dav.example.com/dav" />
          </n-form-item>
          <n-form-item label="用户名">
            <n-input v-model:value="syncCfg.webdav_username" />
          </n-form-item>
          <n-form-item label="密码">
            <n-input v-model:value="syncPassword" type="password" show-password-on="click" placeholder="留空=不修改" />
          </n-form-item>
          <n-form-item label="远程目录">
            <n-input v-model:value="syncCfg.webdav_path" placeholder="ai-proxy-backups/" />
          </n-form-item>
        </n-form>

        <n-space>
          <n-button @click="testSync" :loading="testing">测试连接</n-button>
          <n-button type="primary" @click="saveSyncConfig">保存配置</n-button>
        </n-space>
        <n-text v-if="testResult" :type="testResult.success ? 'success' : 'error'">
          {{ testResult.success ? '✅ 连接成功' : '❌ ' + testResult.error }}
        </n-text>

        <n-divider />
        <n-checkbox v-model:checked="syncCfg.auto_enabled">启用自动同步</n-checkbox>
        <n-form-item label="同步间隔" label-placement="left" :label-width="80">
          <n-select v-model:value="syncCfg.auto_interval_minutes" :options="[
            { label: '每 30 分钟', value: 30 },
            { label: '每 60 分钟', value: 60 },
            { label: '每 3 小时', value: 180 },
            { label: '每 12 小时', value: 720 },
            { label: '每 24 小时', value: 1440 },
          ]" style="width: 180px" />
        </n-form-item>
        <n-checkbox v-model:checked="syncCfg.sync_on_change">配置变更后自动上传</n-checkbox>

        <n-divider />
        <n-text depth="3">
          上次同步: {{ syncLast.last_upload_at || '从未' }}
          {{ syncLast.last_upload_status === 'success' ? '✅成功' : syncLast.last_error ? '❌' + syncLast.last_error : '' }}
        </n-text>
        <n-space>
          <n-button @click="uploadNow" :disabled="!syncCfg.enabled">立即上传</n-button>
          <n-button @click="loadVersions" :disabled="!syncCfg.enabled">管理版本</n-button>
        </n-space>

        <n-collapse-transition :show="showVersions">
          <n-data-table
            :columns="[
              { title: '文件名', key: 'filename' },
              { title: '大小', key: 'size', render: (row: any) => (row.size / 1024).toFixed(1) + ' KB' },
              { title: '修改时间', key: 'modified_at' },
              { title: '操作', key: 'actions', render: (row: any) => h(NSpace, null, {
                  default: () => [
                    h(NButton, { size: 'small', onClick: () => startRemoteRestore(row.filename) }, { default: () => '恢复' }),
                    h(NPopconfirm, { onPositiveClick: () => deleteVersion(row.filename) }, {
                      trigger: () => h(NButton, { size: 'small', type: 'error' }, { default: () => '删除' }),
                      default: () => `确认删除 ${row.filename}?`,
                    }),
                  ]
                }) },
            ]"
            :data="remoteVersions"
            size="small"
          />
        </n-collapse-transition>
      </n-space>
    </n-card>
      </n-space>
    </n-tab-pane>

    <!-- 关于 -->
    <n-tab-pane name="about" tab="关于">
      <n-space vertical size="large">
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
      </n-space>
    </n-tab-pane>
  </n-tabs>

    <!-- 口令设置弹窗 -->
    <n-modal v-model:show="showPassphraseModal" preset="dialog" title="设置备份口令">
      <n-space vertical>
        <n-input v-if="passphraseSet" v-model:value="passphraseForm.old" type="password" placeholder="当前口令" />
        <n-input v-model:value="passphraseForm.new1" type="password" placeholder="新口令 (至少8位)" />
        <n-input v-model:value="passphraseForm.new2" type="password" placeholder="确认新口令" />
        <n-text v-if="passphraseSet" type="warning">⚠ 修改口令后，用旧口令加密的备份将无法用新口令恢复</n-text>
      </n-space>
      <template #action>
        <n-button @click="showPassphraseModal = false">取消</n-button>
        <n-button type="primary" @click="savePassphrase">确认</n-button>
      </template>
    </n-modal>

    <!-- 本地导入确认弹窗 -->
    <n-modal v-model:show="importConfirm.show" preset="dialog" title="恢复备份">
      <n-space vertical>
        <n-text>文件: {{ importConfirm.fileName }}</n-text>
        <n-text type="error">恢复将完全覆盖当前所有配置！此操作不可撤销。</n-text>
        <n-checkbox v-model:checked="importConfirm.agreed">我已了解此操作不可撤销</n-checkbox>
        <n-input v-model:value="importConfirm.passphrase" type="password" placeholder="口令 (本机恢复可留空)" />
      </n-space>
      <template #action>
        <n-button @click="importConfirm.show = false">取消</n-button>
        <n-button type="error" :disabled="!importConfirm.agreed" @click="confirmImport">确认恢复</n-button>
      </template>
    </n-modal>

    <!-- 远程恢复确认弹窗 -->
    <n-modal v-model:show="restoreConfirm.show" preset="dialog" title="从远程恢复">
      <n-space vertical>
        <n-text>文件: {{ restoreConfirm.filename }}</n-text>
        <n-text type="error">恢复将完全覆盖当前所有配置！</n-text>
        <n-checkbox v-model:checked="restoreConfirm.agreed">我已了解此操作不可撤销</n-checkbox>
        <n-input v-model:value="restoreConfirm.passphrase" type="password" placeholder="口令 (本机恢复可留空)" />
      </n-space>
      <template #action>
        <n-button @click="restoreConfirm.show = false">取消</n-button>
        <n-button type="error" :disabled="!restoreConfirm.agreed" @click="confirmRemoteRestore">确认恢复</n-button>
      </template>
    </n-modal>

    <UpdateNotification ref="updateNotification" />
</template>

<script setup lang="ts">
import { ref, reactive, h, onMounted } from 'vue'
import { useMessage, NButton, NSpace, NPopconfirm } from 'naive-ui'
import { invoke } from '@tauri-apps/api/core'
import { isEnabled, enable, disable } from '@tauri-apps/plugin-autostart'
import { getVersion } from '@tauri-apps/api/app'
import { save as saveDialog, open as openDialog } from '@tauri-apps/plugin-dialog'
import { isTauri } from '../utils/env'
import { writeTextFile, readTextFile } from '../utils/tauri-fs'
import { api, refreshApiConfig, backupApi, syncApi } from '../api'
import type { RemoteBackup, SyncConfigResponse } from '../api'
import { useTheme } from '../theme/use-theme'
import type { ThemeMode } from '../theme/use-theme'
import UpdateNotification from '../components/UpdateNotification.vue'

const { mode: currentThemeMode, setMode } = useTheme()
const message = useMessage()
const CLAUDE_CLI_UA = 'claude-cli/2.1.181 (external, cli)'
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
  upstreamInvisibleRetryMode: string
  upstreamInvisibleRetryTotalTimeoutSecs: number
  upstreamInvisibleRetryBufferLimitMb: number
  extractSystemFromMessages: boolean
  upstreamUserAgent: string
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
  upstreamInvisibleRetryMode: 'pre_first_token',
  upstreamInvisibleRetryTotalTimeoutSecs: 600,
  upstreamInvisibleRetryBufferLimitMb: 32,
  extractSystemFromMessages: true,
  upstreamUserAgent: '',
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
      upstream_max_retries: string
      upstream_retry_backoff_base_ms: string
      upstream_invisible_retry_mode: string
      upstream_invisible_retry_total_timeout_secs: string
      upstream_invisible_retry_buffer_limit_mb: string
      extract_system_from_messages: string
      upstream_user_agent: string
    }>('/api/settings')
    settings.value = {
      port: parseInt(data.http_port) || 7860,
      requestTimeout: parseInt(data.request_timeout) || 1200,
      connectTimeout: parseInt(data.connect_timeout) || 30,
      logRetentionDays: parseInt(data.log_retention_days) || 30,
      recordRequestBody: data.record_request_body === 'true',
      proxyAuthKey: data.proxy_auth_key,
      upstreamMaxRetries: parseInt(data.upstream_max_retries) || 10,
      upstreamRetryBackoffBaseMs: parseInt(data.upstream_retry_backoff_base_ms) || 500,
      upstreamInvisibleRetryMode: data.upstream_invisible_retry_mode || 'pre_first_token',
      upstreamInvisibleRetryTotalTimeoutSecs: parseInt(data.upstream_invisible_retry_total_timeout_secs) || 600,
      upstreamInvisibleRetryBufferLimitMb: parseInt(data.upstream_invisible_retry_buffer_limit_mb) || 32,
      extractSystemFromMessages: data.extract_system_from_messages !== 'false',
      upstreamUserAgent: data.upstream_user_agent || '',
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
        upstream_invisible_retry_mode: settings.value.upstreamInvisibleRetryMode,
        upstream_invisible_retry_total_timeout_secs: String(settings.value.upstreamInvisibleRetryTotalTimeoutSecs),
        upstream_invisible_retry_buffer_limit_mb: String(settings.value.upstreamInvisibleRetryBufferLimitMb),
        extract_system_from_messages: String(settings.value.extractSystemFromMessages),
        upstream_user_agent: settings.value.upstreamUserAgent,
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

// --- Backup & Sync ---
const passphraseSet = ref(false)
const showPassphraseModal = ref(false)
const passphraseForm = reactive({ old: '', new1: '', new2: '' })

async function loadBackupStatus() {
  try {
    const r = await backupApi.getStatus()
    passphraseSet.value = r.passphrase_set
  } catch (e) { /* ignore */ }
}

async function savePassphrase() {
  if (passphraseForm.new1.length < 8) { message.error('口令至少需要 8 位'); return }
  if (passphraseForm.new1 !== passphraseForm.new2) { message.error('两次输入不一致'); return }
  try {
    await backupApi.setPassphrase(passphraseForm.new1, passphraseForm.old || undefined)
    message.success('口令已设置')
    showPassphraseModal.value = false
    passphraseForm.old = passphraseForm.new1 = passphraseForm.new2 = ''
    await loadBackupStatus()
  } catch (e: any) { message.error(e.message || '设置失败') }
}

async function exportBackup() {
  try {
    const r = await backupApi.exportBackup()
    const bytes = Uint8Array.from(atob(r.data), c => c.charCodeAt(0))
    if (isTauri) {
      const filename = `ai-proxy-backup-${new Date().toISOString().replace(/[:.]/g, '-')}.json`
      const path = await saveDialog({ defaultPath: filename, filters: [{ name: 'JSON', extensions: ['json'] }] })
      if (path) {
        await writeTextFile(path, r.data)
        message.success('已导出')
      }
    } else {
      // Browser: trigger download
      const blob = new Blob([bytes], { type: 'application/json' })
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url; a.download = 'ai-proxy-backup.json'; a.click()
      URL.revokeObjectURL(url)
      message.success('已导出')
    }
  } catch (e: any) { message.error(e.message || '导出失败') }
}

const importConfirm = reactive({ show: false, fileName: '', fileData: '', passphrase: '', agreed: false })

async function pickImportFile() {
  try {
    if (isTauri) {
      const path = await openDialog({ filters: [{ name: 'JSON', extensions: ['json'] }] })
      if (path && typeof path === 'string') {
        const data = await readTextFile(path)
        importConfirm.fileName = path.split(/[\\/]/).pop() || path
        importConfirm.fileData = data
        importConfirm.show = true
        importConfirm.agreed = false
      }
    } else {
      const input = document.createElement('input')
      input.type = 'file'; input.accept = '.json'
      input.onchange = async () => {
        const f = input.files?.[0]; if (!f) return
        const text = await f.text()
        importConfirm.fileName = f.name
        importConfirm.fileData = text
        importConfirm.show = true
        importConfirm.agreed = false
      }
      input.click()
    }
  } catch (e: any) { message.error(e.message || '读取文件失败') }
}

async function confirmImport() {
  if (!importConfirm.agreed) { message.warning('请先勾选确认'); return }
  try {
    await backupApi.importBackup(importConfirm.fileData, importConfirm.passphrase || undefined)
    message.success('恢复成功，即将刷新页面')
    importConfirm.show = false
    setTimeout(() => window.location.reload(), 1200)
  } catch (e: any) { message.error(e.message || '恢复失败') }
}

// --- Sync ---
const syncCfg = ref<SyncConfigResponse>({
  enabled: false, webdav_url: '', webdav_username: '', webdav_path: 'ai-proxy-backups/',
  auto_enabled: false, auto_interval_minutes: 60, sync_on_change: false,
})
const syncPassword = ref('')
const syncLast = ref({ last_upload_at: '', last_upload_status: '', last_error: '' })
const remoteVersions = ref<RemoteBackup[]>([])
const showVersions = ref(false)
const testing = ref(false)
const testResult = ref<{ success: boolean; error?: string } | null>(null)

async function loadSyncConfig() {
  try {
    syncCfg.value = await syncApi.getConfig()
    const last = await syncApi.getLast()
    syncLast.value = last
  } catch (e) { /* ignore */ }
}

async function saveSyncConfig() {
  try {
    await syncApi.saveConfig({
      enabled: syncCfg.value.enabled,
      webdav_url: syncCfg.value.webdav_url,
      webdav_username: syncCfg.value.webdav_username,
      webdav_password: syncPassword.value,
      webdav_path: syncCfg.value.webdav_path,
      auto_enabled: syncCfg.value.auto_enabled,
      auto_interval_minutes: syncCfg.value.auto_interval_minutes,
      sync_on_change: syncCfg.value.sync_on_change,
    })
    message.success('同步配置已保存')
    syncPassword.value = ''
  } catch (e: any) { message.error(e.message || '保存失败') }
}

async function testSync() {
  testing.value = true; testResult.value = null
  try {
    // Save first so test uses latest creds
    await saveSyncConfig()
    testResult.value = await syncApi.testConnection()
  } catch (e: any) { testResult.value = { success: false, error: e.message } }
  finally { testing.value = false }
}

async function uploadNow() {
  try {
    const r = await syncApi.upload()
    message.success(`已上传 ${r.filename} (${r.size} bytes)`)
    await loadSyncConfig()
  } catch (e: any) { message.error(e.message || '上传失败') }
}

async function loadVersions() {
  try { remoteVersions.value = await syncApi.listVersions(); showVersions.value = true }
  catch (e: any) { message.error(e.message || '获取版本失败') }
}

const restoreConfirm = reactive({ show: false, filename: '', passphrase: '', agreed: false })
function startRemoteRestore(filename: string) {
  restoreConfirm.filename = filename; restoreConfirm.passphrase = ''; restoreConfirm.agreed = false
  restoreConfirm.show = true
}
async function confirmRemoteRestore() {
  if (!restoreConfirm.agreed) { message.warning('请先勾选确认'); return }
  try {
    await syncApi.restore(restoreConfirm.filename, restoreConfirm.passphrase || undefined)
    message.success('恢复成功，即将刷新页面')
    restoreConfirm.show = false
    setTimeout(() => window.location.reload(), 1200)
  } catch (e: any) { message.error(e.message || '恢复失败') }
}

async function deleteVersion(filename: string) {
  try {
    await syncApi.deleteVersion(filename)
    message.success('已删除')
    await loadVersions()
  } catch (e: any) { message.error(e.message || '删除失败') }
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
  loadBackupStatus()
  loadSyncConfig()
})
</script>
