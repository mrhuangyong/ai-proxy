<template>
  <n-space vertical size="large">
    <n-card>
      <template #header>
        <n-text strong>应用管理</n-text>
      </template>

      <n-spin :show="loading">
        <div class="app-grid">
          <n-card
            v-for="app in apps"
            :key="app.app_type"
            size="small"
            hoverable
            class="app-card"
            :data-type="app.app_type"
          >
            <template #header>
              <n-space justify="space-between" align="center">
                <n-text strong>{{ displayName(app.app_type) }}</n-text>
                <n-button
                  quaternary
                  size="small"
                  @click="openPathModal(app)"
                >
                  <template #icon>
                    <n-icon><SettingsOutline /></n-icon>
                  </template>
                </n-button>
              </n-space>
            </template>

            <div class="app-card-content">
              <n-space vertical size="small">
                <n-tag
                  :type="app.installed ? 'success' : 'error'"
                  size="small"
                >
                  {{ app.installed ? '已安装' : '未安装' }}
                </n-tag>

                <n-text
                  v-if="app.install_path"
                  depth="3"
                  style="font-size: 12px; word-break: break-all"
                >
                  {{ app.install_path }}
                </n-text>

                <n-tag v-if="app.model" size="small" type="info">
                  {{ app.model }}
                </n-tag>

                <n-text
                  v-if="app.launched_at"
                  depth="3"
                  style="font-size: 12px"
                >
                  上次启动: {{ formatTime(app.launched_at) }}
                </n-text>
              </n-space>

              <div class="app-card-action">
                <n-button
                  type="primary"
                  size="small"
                  block
                  :disabled="!app.installed"
                  @click="openLaunchModal(app)"
                >
                  打开
                </n-button>
              </div>
            </div>
          </n-card>
        </div>
      </n-spin>
    </n-card>

    <n-modal
      v-model:show="showLaunchModal"
      preset="dialog"
      title="启动应用"
      positive-text="启动"
      negative-text="取消"
      :loading="launchLoading"
      @positive-click="handleLaunch"
      style="width: 520px"
    >
      <n-space vertical size="medium">
        <n-text>应用: {{ launchForm.appName }}</n-text>

        <template v-if="isOpencodeApp(launchForm.appType)">
          <n-space vertical size="small" style="width: 100%">
            <n-text depth="3" style="font-size: 13px">注入模型（可多选）</n-text>
            <n-select
              v-model:value="launchForm.models"
              :options="modelOptions"
              multiple
              filterable
              tag
              placeholder="选择要注入到 opencode 配置的模型"
            />
          </n-space>
        </template>
        <template v-else>
          <n-space vertical size="small" style="width: 100%">
            <n-text depth="3" style="font-size: 13px">默认模型</n-text>
            <n-select
              v-model:value="launchForm.model"
              :options="modelOptions"
              filterable
              tag
              placeholder="选择或输入模型"
            />
          </n-space>
        </template>

        <template v-if="isCodexApp(launchForm.appType)">
          <n-space vertical size="small" style="width: 100%">
            <n-text depth="3" style="font-size: 13px">可见模型（可多选）</n-text>
            <n-select
              v-model:value="launchForm.visibleModels"
              :options="modelOptions"
              multiple
              filterable
              tag
              clearable
              placeholder="留空则显示代理全部模型"
            />
            <n-text depth="3" style="font-size: 12px">
              选中后注入 codex 模型目录（model_catalog_json），/model 选择器只显示这些模型；默认模型始终包含
            </n-text>
          </n-space>
        </template>

        <template v-if="isCodexApp(launchForm.appType)">
          <n-space vertical size="small" style="width: 100%">
            <n-text depth="3" style="font-size: 13px">登录凭证</n-text>
            <n-radio-group v-model:value="launchForm.authMode" name="codex-auth-mode">
              <n-space vertical size="small">
                <n-radio value="apikey">代理 API Key（覆盖 auth.json）</n-radio>
                <n-radio value="preserve">保留本机登录（不改 auth.json）</n-radio>
                <n-radio value="inject" :disabled="!codexAccountOptions.length">
                  注入官方账号
                </n-radio>
              </n-space>
            </n-radio-group>
            <n-text depth="3" style="font-size: 12px">
              <template v-if="launchForm.authMode === 'inject'">
                将供应商管理中已登录的 Codex 官方凭证写入 auth.json，保留手机远程和官方插件
              </template>
              <template v-else-if="launchForm.authMode === 'preserve'">
                仅修改 config.toml，不覆盖 auth.json
              </template>
              <template v-else>
                写入代理 API Key 到 auth.json（会清除本机 ChatGPT tokens）
              </template>
            </n-text>
            <template v-if="launchForm.authMode === 'inject'">
              <n-select
                v-model:value="launchForm.injectProviderId"
                :options="codexAccountOptions"
                placeholder="选择要注入的 Codex 官方账号"
                filterable
              />
            </template>
            <n-text
              v-else-if="!codexAccountOptions.length"
              depth="3"
              style="font-size: 12px"
            >
              暂无已登录账号：请先在供应商管理登录 Codex 官方账号后再注入
            </n-text>
          </n-space>
        </template>

        <template v-if="isClaudeApp(launchForm.appType)">
          <n-space vertical size="small" style="width: 100%">
            <n-text depth="3" style="font-size: 13px">Haiku 模型（可选）</n-text>
            <n-select
              v-model:value="launchForm.model_haiku"
              :options="modelOptionsWithEmpty"
              filterable
              tag
              clearable
              placeholder="选择或输入 Haiku 模型"
            />
          </n-space>

          <n-space vertical size="small" style="width: 100%">
            <n-text depth="3" style="font-size: 13px">Sonnet 模型（可选）</n-text>
            <n-select
              v-model:value="launchForm.model_sonnet"
              :options="modelOptionsWithEmpty"
              filterable
              tag
              clearable
              placeholder="选择或输入 Sonnet 模型"
            />
          </n-space>

          <n-space vertical size="small" style="width: 100%">
            <n-text depth="3" style="font-size: 13px">Opus 模型（可选）</n-text>
            <n-select
              v-model:value="launchForm.model_opus"
              :options="modelOptionsWithEmpty"
              filterable
              tag
              clearable
              placeholder="选择或输入 Opus 模型"
            />
          </n-space>
        </template>

        <template v-if="isCliApp(launchForm.appType)">
          <n-space vertical size="small" style="width: 100%">
            <n-text depth="3" style="font-size: 13px">工作目录</n-text>
            <n-input-group>
              <n-input
                v-model:value="launchForm.work_dir"
                placeholder="留空则使用默认目录"
                style="flex: 1"
              />
              <n-button @click="browseWorkDir">
                浏览
              </n-button>
            </n-input-group>
          </n-space>
        </template>
      </n-space>
    </n-modal>

    <n-modal
      v-model:show="showPathModal"
      preset="dialog"
      title="配置安装路径"
      positive-text="保存"
      negative-text="取消"
      :loading="pathLoading"
      @positive-click="handleSetPath"
      style="width: 480px"
    >
      <n-space vertical size="medium">
        <n-text>应用: {{ pathForm.appName }}</n-text>
        <n-input-group>
          <n-input
            v-model:value="pathForm.install_path"
            placeholder="留空则自动检测"
            style="flex: 1"
          />
          <n-button @click="browseInstallPath">
            浏览
          </n-button>
        </n-input-group>
      </n-space>
    </n-modal>
  </n-space>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useMessage } from 'naive-ui'
import { useRouter } from 'vue-router'
import { SettingsOutline } from '@vicons/ionicons5'
import { open } from '@tauri-apps/plugin-dialog'
import { api } from '../api'
import { formatDateTime } from '../utils/format'
import type { AppConfig, AppType, CodexAuthMode, Provider, ProviderModel, VirtualModel } from '../types'

const message = useMessage()
const router = useRouter()
const loading = ref(false)
const apps = ref<AppConfig[]>([])
const allModels = ref<ProviderModel[]>([])
const allProviders = ref<Provider[]>([])
const virtualModelNames = ref<string[]>([])

const showLaunchModal = ref(false)
const launchLoading = ref(false)
const launchForm = ref({
  appType: '' as AppType,
  appName: '',
  model: '',
  model_haiku: null as string | null,
  model_sonnet: null as string | null,
  model_opus: null as string | null,
  work_dir: '',
  authMode: 'apikey' as CodexAuthMode,
  injectProviderId: null as string | null,
  models: [] as string[],
  visibleModels: [] as string[],
})

const showPathModal = ref(false)
const pathLoading = ref(false)
const pathForm = ref({
  appType: '' as AppType,
  appName: '',
  install_path: '',
})

const displayNameMap: Record<AppType, string> = {
  codex_cli: 'Codex CLI',
  codex_desktop: 'Codex Desktop',
  claude_cli: 'Claude CLI',
  claude_desktop: 'Claude Desktop',
  opencode_cli: 'OpenCode CLI',
}

function displayName(appType: AppType): string {
  return displayNameMap[appType] || appType
}

function formatTime(iso: string): string {
  return formatDateTime(iso)
}

function isClaudeApp(appType: AppType): boolean {
  return appType === 'claude_cli' || appType === 'claude_desktop'
}

function isCliApp(appType: AppType): boolean {
  return appType === 'codex_cli' || appType === 'claude_cli' || appType === 'opencode_cli'
}

function isCodexApp(appType: AppType): boolean {
  return appType === 'codex_cli' || appType === 'codex_desktop'
}

function isOpencodeApp(appType: AppType): boolean {
  return appType === 'opencode_cli'
}

async function browseInstallPath() {
  const selected = await open({ multiple: false })
  if (typeof selected === 'string') {
    pathForm.value.install_path = selected
  }
}

async function browseWorkDir() {
  const selected = await open({ directory: true, multiple: false })
  if (typeof selected === 'string') {
    launchForm.value.work_dir = selected
  }
}

const modelOptions = computed(() => {
  const seen = new Set<string>()
  const options: { label: string; value: string }[] = []
  for (const m of allModels.value) {
    if (!seen.has(m.model_name)) {
      seen.add(m.model_name)
      options.push({ label: m.model_name, value: m.model_name })
    }
  }
  // Append virtual models with a [虚拟] tag so users can distinguish them.
  for (const vn of virtualModelNames.value) {
    if (!seen.has(vn)) {
      seen.add(vn)
      options.push({ label: `${vn} [虚拟]`, value: vn })
    }
  }
  return options
})

const modelOptionsWithEmpty = computed(() => {
  return modelOptions.value
})

/** Logged-in Codex OAuth accounts available for inject mode. */
const codexAccountOptions = computed(() => {
  return allProviders.value
    .filter(
      (p) =>
        p.provider_kind === 'codex_oauth' &&
        p.codex_account &&
        p.codex_account.status === 'active',
    )
    .map((p) => {
      const acc = p.codex_account!
      const who = acc.email || acc.chatgpt_account_id || '未知账号'
      const plan = acc.plan_type ? ` · ${acc.plan_type}` : ''
      return {
        label: `${p.name}: ${who}${plan}`,
        value: p.id,
      }
    })
})

async function fetchApps() {
  loading.value = true
  try {
    apps.value = (await api<AppConfig[]>('/api/apps'))

  } catch (err) {
    console.error('Failed to load apps:', err)
  } finally {
    loading.value = false
  }
}

async function fetchModels() {
  try {
    const providers = await api<Provider[]>('/api/providers')
    allProviders.value = providers
    allModels.value = providers.flatMap((p) => p.models)
  } catch (err) {
    console.error('Failed to load models:', err)
  }
  // Also load virtual model names so they appear in the dropdown.
  try {
    const vms = await api<VirtualModel[]>('/api/virtual-models')
    virtualModelNames.value = vms.filter((v) => v.enabled).map((v) => v.name)
  } catch {
    // virtual-models API may not be available; silently ignore
  }
}

async function openLaunchModal(app: AppConfig) {
  try {
    const s = await api<{ proxy_auth_key?: string }>('/api/settings')
    if (!s.proxy_auth_key) {
      message.warning('请先在设置中配置代理 API Key')
      router.push('/settings')
      return
    }
  } catch {
    // ignore
  }

  // 检查对应 provider 是否有 API key
  await fetchModels()
  const requiredProvider = app.app_type === 'claude_cli' || app.app_type === 'claude_desktop'
    ? 'anthropic'
    : null
  if (requiredProvider) {
    const provider = allProviders.value.find(p => p.format === requiredProvider)
    if (!provider || provider.api_keys.length === 0) {
      message.warning('请先在设置中配置 Anthropic API Key')
      router.push('/settings')
      return
    }
  }

  launchForm.value = {
    appType: app.app_type,
    appName: displayName(app.app_type),
    model: app.model || '',
    model_haiku: app.model_haiku || null,
    model_sonnet: app.model_sonnet || null,
    model_opus: app.model_opus || null,
    work_dir: app.work_dir || '',
    authMode: 'apikey',
    injectProviderId: null,
    models: app.opencode_models || [],
    visibleModels: app.visible_models || [],
  }

  if (isCodexApp(app.app_type)) {
    try {
      const s = await api<{
        codex_auth_mode?: string
        codex_preserve_auth?: string
        codex_inject_provider_id?: string
      }>('/api/settings')
      const mode = s.codex_auth_mode
      if (mode === 'apikey' || mode === 'preserve' || mode === 'inject') {
        launchForm.value.authMode = mode
      } else if (s.codex_preserve_auth === 'true') {
        launchForm.value.authMode = 'preserve'
      }
      const savedPid = s.codex_inject_provider_id || ''
      if (savedPid && codexAccountOptions.value.some((o) => o.value === savedPid)) {
        launchForm.value.injectProviderId = savedPid
      } else if (codexAccountOptions.value.length === 1) {
        launchForm.value.injectProviderId = codexAccountOptions.value[0].value
      }
      // Inject selected but no accounts available → fall back to preserve.
      if (launchForm.value.authMode === 'inject' && !codexAccountOptions.value.length) {
        launchForm.value.authMode = 'preserve'
      }
    } catch {
      // ignore
    }
  }
  showLaunchModal.value = true
}

async function handleLaunch() {
  if (isOpencodeApp(launchForm.value.appType)) {
    if (!launchForm.value.models.length) {
      message.warning('请至少选择一个模型')
      return false
    }
  } else if (!launchForm.value.model) {
    message.warning('请选择或输入默认模型')
    return false
  }

  if (isCodexApp(launchForm.value.appType) && launchForm.value.authMode === 'inject') {
    if (!launchForm.value.injectProviderId) {
      message.warning('请选择要注入的 Codex 官方账号')
      return false
    }
  }

  launchLoading.value = true
  try {
    if (isCodexApp(launchForm.value.appType)) {
      await api<void>('/api/settings', {
        method: 'PUT',
        body: JSON.stringify({
          codex_auth_mode: launchForm.value.authMode,
          codex_preserve_auth: launchForm.value.authMode === 'preserve' ? 'true' : 'false',
          codex_inject_provider_id: launchForm.value.injectProviderId || '',
        }),
      })
    }

    const body: Record<string, unknown> = {
      app_type: launchForm.value.appType,
      model: launchForm.value.model,
    }

    if (launchForm.value.model_haiku) {
      body.model_haiku = launchForm.value.model_haiku
    }
    if (launchForm.value.model_sonnet) {
      body.model_sonnet = launchForm.value.model_sonnet
    }
    if (launchForm.value.model_opus) {
      body.model_opus = launchForm.value.model_opus
    }
    if (launchForm.value.work_dir.trim()) {
      body.work_dir = launchForm.value.work_dir.trim()
    }
    if (isOpencodeApp(launchForm.value.appType) && launchForm.value.models.length) {
      body.models = launchForm.value.models
      // Use first selected model as the "model" field for DB storage
      body.model = launchForm.value.models[0]
    }
    if (isCodexApp(launchForm.value.appType)) {
      // 空数组也要提交：表示清空目录限制，后端会删除 model_catalog_json。
      body.visible_models = launchForm.value.visibleModels
      body.codex_auth_mode = launchForm.value.authMode
      if (launchForm.value.authMode === 'inject' && launchForm.value.injectProviderId) {
        body.codex_account_provider_id = launchForm.value.injectProviderId
      }
    }

    await api<void>('/api/apps/launch', {
      method: 'POST',
      body: JSON.stringify(body),
    })
    message.success('应用启动成功')
    showLaunchModal.value = false
    await fetchApps()
  } catch (err) {
    message.error(`启动失败: ${err}`)
  } finally {
    launchLoading.value = false
  }
  return false
}

function openPathModal(app: AppConfig) {
  pathForm.value = {
    appType: app.app_type,
    appName: displayName(app.app_type),
    install_path: app.install_path || '',
  }
  showPathModal.value = true
}

async function handleSetPath() {
  pathLoading.value = true
  try {
    await api<void>(`/api/apps/${pathForm.value.appType}/path`, {
      method: 'PUT',
      body: JSON.stringify({
        install_path: pathForm.value.install_path,
      }),
    })
    message.success('路径配置已保存')
    showPathModal.value = false
    await fetchApps()
  } catch (err) {
    message.error(`保存失败: ${err}`)
  } finally {
    pathLoading.value = false
  }
  return false
}

onMounted(() => {
  fetchApps()
  fetchModels()
})
</script>

<style scoped>
.app-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
  gap: 16px;
}

.app-grid > :deep(.n-card) {
  display: flex;
  flex-direction: column;
}

.app-grid :deep(.n-card-content) {
  display: flex;
  flex-direction: column;
  flex: 1;
}

.app-card-content {
  display: flex;
  flex-direction: column;
  flex: 1;
}

.app-card-action {
  margin-top: auto;
  padding-top: 8px;
}
</style>
