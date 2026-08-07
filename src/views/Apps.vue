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
          <n-space justify="space-between" align="center">
            <n-space vertical size="small">
              <n-text depth="3" style="font-size: 13px">保留官方登录</n-text>
              <n-text depth="3" style="font-size: 12px">启用后仅修改 config.toml，不覆盖 auth.json，保留手机远程和官方插件</n-text>
            </n-space>
            <n-switch v-model:value="launchForm.preserveAuth" />
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
import type { AppConfig, AppType, Provider, ProviderModel, VirtualModel } from '../types'

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
  preserveAuth: false,
  models: [] as string[],
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
    preserveAuth: false,
    models: app.opencode_models || [],
  }

  if (isCodexApp(app.app_type)) {
    try {
      const s = await api<{ codex_preserve_auth?: string }>('/api/settings')
      launchForm.value.preserveAuth = s.codex_preserve_auth === 'true'
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

  launchLoading.value = true
  try {
    // Save codex_preserve_auth setting before launching
    if (isCodexApp(launchForm.value.appType)) {
      await api<void>('/api/settings', {
        method: 'PUT',
        body: JSON.stringify({
          codex_preserve_auth: launchForm.value.preserveAuth ? 'true' : 'false',
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
