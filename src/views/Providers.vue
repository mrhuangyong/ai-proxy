<template>
  <n-space vertical size="large">
    <div class="provider-header">
      <n-text strong style="font-size: 16px">供应商管理</n-text>
      <n-button type="primary" size="small" @click="openCreateModal">
        添加供应商
      </n-button>
    </div>

    <n-spin :show="loading">
      <div class="provider-grid">
        <div
          v-for="p in providers"
          :key="p.id"
          class="provider-card"
          :class="{ 'provider-card--disabled': !p.enabled }"
          :data-format="p.format"
        >
          <div class="provider-card-bar" />
          <div class="provider-card-header">
            <div class="provider-card-title">
              <span class="provider-card-name">{{ p.name }}</span>
              <n-tag size="small" :type="formatColorMap[p.format] || 'default'" round>
                {{ p.format }}
              </n-tag>
            </div>
            <n-switch :value="p.enabled" size="small" @update:value="(v: boolean) => handleToggle(p.id, v)" />
          </div>

          <n-text depth="3" class="provider-card-url font-mono">{{ p.base_url }}</n-text>

          <div class="provider-card-models">
            <template v-if="p.models.length">
              <n-tag
                v-for="m in p.models"
                :key="m.model_name"
                size="small"
                round
                :bordered="false"
                type="default"
                class="provider-model-tag"
              >
                {{ m.model_name }}
              </n-tag>
            </template>
            <n-text v-else depth="3" style="font-size: 12px">暂无模型</n-text>
          </div>

          <div class="provider-card-actions">
            <n-button quaternary size="tiny" type="info" @click="openTestModal(p)">
              测试
            </n-button>
            <n-button quaternary size="tiny" type="primary" @click="openEditModal(p)">
              编辑
            </n-button>
            <n-popconfirm @positive-click="handleDelete(p.id)">
              <template #trigger>
                <n-button quaternary size="tiny" type="error">删除</n-button>
              </template>
              确认删除此供应商？
            </n-popconfirm>
          </div>
        </div>
      </div>

      <n-empty v-if="!loading && providers.length === 0" description="暂无供应商" style="padding: 48px 0" />
    </n-spin>

    <n-modal
      v-model:show="showModal"
      preset="dialog"
      :title="isEditing ? '编辑供应商' : '添加供应商'"
      positive-text="确认"
      negative-text="取消"
      :loading="modalLoading"
      @positive-click="handleSubmit"
      style="width: 600px"
    >
      <n-form :model="form" label-placement="left" label-width="80">
        <n-form-item label="名称" required>
          <n-input v-model:value="form.name" placeholder="例如: opencode" :input-props="{ autocapitalize: 'off' }" />
          <template #feedback>
            <n-text v-if="form.name && !/^[A-Za-z][A-Za-z0-9]*$/.test(form.name)" type="error" style="font-size: 12px">
              仅支持英文字母与数字，必须以字母开头
            </n-text>
          </template>
        </n-form-item>
        <n-form-item label="Base URL" required>
          <n-input v-model:value="form.base_url" placeholder="例如: https://api.openai.com" :input-props="{ autocapitalize: 'off' }" />
        </n-form-item>
        <n-form-item label="格式">
          <n-select
            v-model:value="form.format"
            :options="formatOptions"
            placeholder="选择格式"
          />
        </n-form-item>
        <n-form-item label="Endpoint">
          <n-input
            v-model:value="form.endpoint_path"
            placeholder="留空使用默认路径，例如: /chat/completions"
            :input-props="{ autocapitalize: 'off' }"
          />
        </n-form-item>
        <n-form-item label="上游 User-Agent">
          <n-input-group>
            <n-input
              v-model:value="form.upstream_user_agent"
              placeholder="留空则用全局 UA"
              :input-props="{ autocapitalize: 'off' }"
            />
            <n-button @click="form.upstream_user_agent = CLAUDE_CLI_UA">
              Claude CLI
            </n-button>
          </n-input-group>
        </n-form-item>
        <n-form-item label="API Key" :required="!isEditing">
          <n-input
            v-model:value="form.api_key"
            type="password"
            show-password-on="click"
            :placeholder="isEditing ? '留空则不修改' : '输入 API Key'"
          />
        </n-form-item>

        <n-divider>模型列表</n-divider>

        <n-space vertical size="small">
          <div
            v-for="(_, index) in form.models"
            :key="index"
            style="display: flex; align-items: center; gap: 8px"
          >
            <n-input
              v-model:value="form.models[index].model_name"
              placeholder="模型名称"
              style="flex: 1"
            />
            <n-input-number
              v-model:value="form.models[index].context_window"
              placeholder="上下文窗口"
              :min="1000"
              :step="1000"
              style="width: 160px"
            >
              <template #suffix>tokens</template>
            </n-input-number>
            <n-button
              quaternary
              size="small"
              :type="hasNonPermissiveCaps(index) ? 'warning' : 'default'"
              @click="openCapModal(index)"
            >
              能力
            </n-button>
            <n-button
              quaternary
              type="error"
              size="small"
              @click="removeModel(index)"
            >
              删除
            </n-button>
          </div>
          <n-button dashed size="small" @click="addModel">
            + 添加模型
          </n-button>
        </n-space>
      </n-form>
    </n-modal>

    <n-modal
      v-model:show="showCapModal"
      preset="card"
      title="模型能力 — Failover 参数兼容"
      style="width: 560px"
    >
      <n-space vertical>
        <n-alert type="info" :bordered="false">
          关闭某项能力后，轮换到该模型时会自动剥离对应参数，避免上游 4xx。
          默认全开（与历史行为一致）。
        </n-alert>
        <template v-if="capEditingIndex !== null && form.models[capEditingIndex]">
          <div style="display: grid; grid-template-columns: 1fr 1fr; gap: 8px 24px; margin-top: 8px">
            <div
              v-for="cap in CAP_TOGGLES"
              :key="cap.key"
              style="display: flex; align-items: center; justify-content: space-between"
            >
              <span style="font-size: 13px">{{ cap.label }}</span>
              <n-switch
                :value="!!form.models[capEditingIndex].capabilities[cap.key]"
                @update:value="(v: boolean | string) => (form.models[capEditingIndex!].capabilities[cap.key] = !!v as never)"
              />
            </div>
          </div>
          <n-divider style="margin: 12px 0" />
          <div style="display: flex; align-items: center; gap: 12px">
            <span style="font-size: 13px; min-width: 140px">max_tokens 上限</span>
            <n-input-number
              v-model:value="form.models[capEditingIndex].capabilities.max_output_tokens"
              placeholder="不限"
              :min="1"
              :step="1024"
              style="flex: 1"
              clearable
            >
              <template #suffix>tokens</template>
            </n-input-number>
          </div>
          <div style="display: flex; justify-content: flex-end; gap: 8px; margin-top: 16px">
            <n-button size="small" @click="resetCaps(capEditingIndex)">恢复默认</n-button>
            <n-button size="small" type="primary" @click="showCapModal = false">完成</n-button>
          </div>
        </template>
      </n-space>
    </n-modal>

    <n-modal
      v-model:show="showTestModal"
      preset="card"
      :title="`测试模型 - ${testProviderName}`"
      style="width: 600px"
    >
      <n-spin :show="testing">
        <template v-if="testResult">
          <n-result :status="testResult.success ? 'success' : 'error'"
            :title="testResult.message"
          >
            <template #footer>
              <n-descriptions label-placement="left" bordered :column="1" size="small">
                <n-descriptions-item v-if="testResult.duration_ms != null" label="响应时间">
                  {{ testResult.duration_ms }}ms
                </n-descriptions-item>
                <n-descriptions-item v-if="testResult.response_text" label="模型回复">
                  {{ testResult.response_text }}
                </n-descriptions-item>
                <n-descriptions-item v-if="testResult.error" label="错误信息">
                  <n-text type="error">{{ testResult.error }}</n-text>
                </n-descriptions-item>
              </n-descriptions>
            </template>
          </n-result>
        </template>
        <n-space vertical v-if="testModels.length > 0">
          <div
            v-for="m in testModels"
            :key="m.model_name"
            style="display: flex; align-items: center; gap: 8px"
          >
            <n-tag size="small" style="min-width: 120px">{{ m.model_name }}<span v-if="m.context_window && m.context_window !== 272000" style="margin-left: 4px; opacity: 0.6">{{ (m.context_window / 1000).toFixed(0) }}K</span></n-tag>
            <n-button
              size="small"
              type="primary"
              secondary
              :loading="testingModel === m.model_name"
              @click="handleTestModel(m.model_name)"
            >
              测试
            </n-button>
          </div>
        </n-space>
        <n-empty v-else description="该供应商暂无模型" />
      </n-spin>
    </n-modal>
  </n-space>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useMessage } from 'naive-ui'
import { api } from '../api'
import type { Provider, ModelCapabilities } from '../types'

interface TestResult {
  success: boolean
  message: string
  response_text: string | null
  duration_ms: number | null
  error: string | null
}

const message = useMessage()
const loading = ref(false)
const providers = ref<Provider[]>([])
const showModal = ref(false)
const isEditing = ref(false)
const editingId = ref('')
const modalLoading = ref(false)

const showTestModal = ref(false)
const testProviderName = ref('')
const testProviderId = ref('')
const testModels = ref<Array<{ model_name: string; context_window: number | null }>>([])
const testing = ref(false)
const testingModel = ref('')
const testResult = ref<TestResult | null>(null)

const CLAUDE_CLI_UA = 'claude-cli/2.1.181 (external, cli)'

/** Permissive defaults for a model's capability flags — matches the DB
 * defaults (migration 026) so unconfigured models behave as before. */
function defaultCapabilities(): ModelCapabilities {
  return {
    supports_thinking: true,
    supports_tools: true,
    supports_temperature: true,
    supports_top_p: true,
    supports_top_k: true,
    supports_presence_penalty: true,
    supports_frequency_penalty: true,
    supports_seed: true,
    supports_response_format: true,
    supports_stream_options: true,
    supports_stop: true,
    max_output_tokens: null,
    extra_passthrough: true,
  }
}

/** List of capability toggles for the editor popover. `label` is the user-
 * facing name; `key` is the ModelCapabilities field. */
const CAP_TOGGLES: Array<{ key: keyof ModelCapabilities; label: string }> = [
  { key: 'supports_thinking', label: '思考 / 推理' },
  { key: 'supports_tools', label: '工具调用' },
  { key: 'supports_temperature', label: 'temperature' },
  { key: 'supports_top_p', label: 'top_p' },
  { key: 'supports_top_k', label: 'top_k' },
  { key: 'supports_presence_penalty', label: 'presence_penalty' },
  { key: 'supports_frequency_penalty', label: 'frequency_penalty' },
  { key: 'supports_seed', label: 'seed' },
  { key: 'supports_response_format', label: 'response_format' },
  { key: 'supports_stream_options', label: 'stream_options' },
  { key: 'supports_stop', label: 'stop' },
  { key: 'extra_passthrough', label: '透传未知参数 (extra)' },
]

const form = ref({
  name: '',
  base_url: '',
  format: 'completions' as string,
  endpoint_path: '',
  upstream_user_agent: '',
  api_key: '',
  models: [] as Array<{
    id?: string
    model_name: string
    target_model?: string | null
    context_window: number | null
    capabilities: ModelCapabilities
  }>,
})

const formatOptions = [
  { label: 'OpenAI Completions', value: 'completions' },
  { label: 'OpenAI Responses', value: 'responses' },
  { label: 'Anthropic', value: 'anthropic' },
]

const formatColorMap: Record<string, string> = {
  completions: 'success',
  responses: 'info',
  anthropic: 'warning',
  gemini: 'purple',
}

function addModel() {
  form.value.models = [
    ...form.value.models,
    { model_name: '', context_window: null, capabilities: defaultCapabilities() },
  ]
}

function removeModel(index: number) {
  form.value.models = form.value.models.filter((_, i) => i !== index)
}

function openCreateModal() {
  isEditing.value = false
  editingId.value = ''
  form.value = {
    name: '',
    base_url: '',
    format: 'completions',
    endpoint_path: '',
    upstream_user_agent: '',
    api_key: '',
    models: [],
  }
  showModal.value = true
}

function openEditModal(row: Provider) {
  isEditing.value = true
  editingId.value = row.id
  form.value = {
    name: row.name,
    base_url: row.base_url,
    format: row.format,
    endpoint_path: row.endpoint_path || '',
    upstream_user_agent: row.upstream_user_agent || '',
    api_key: '',
    models: row.models.map((m) => ({
      id: m.id,
      model_name: m.model_name,
      target_model: m.target_model ?? null,
      context_window: m.context_window ?? null,
      capabilities: m.capabilities ?? defaultCapabilities(),
    })),
  }
  showModal.value = true
}

function openTestModal(row: Provider) {
  testProviderName.value = row.name
  testProviderId.value = row.id
  testModels.value = row.models.map((m) => ({ model_name: m.model_name, context_window: m.context_window ?? null }))
  testResult.value = null
  testingModel.value = ''
  showTestModal.value = true
}

// ---- Capability editor ----
const showCapModal = ref(false)
const capEditingIndex = ref<number | null>(null)

function openCapModal(index: number) {
  capEditingIndex.value = index
  showCapModal.value = true
}

/** True when a model's capabilities differ from the permissive defaults —
 * used to highlight the "能力" button so the user can see which models have
 * custom parameter filtering applied. */
function hasNonPermissiveCaps(index: number): boolean {
  const caps = form.value.models[index]?.capabilities
  if (!caps) return false
  const def = defaultCapabilities()
  return (Object.keys(def) as Array<keyof ModelCapabilities>).some((k) => {
    if (k === 'max_output_tokens') {
      return caps[k] !== null
    }
    return caps[k] !== def[k]
  })
}

function resetCaps(index: number) {
  if (form.value.models[index]) {
    form.value.models[index].capabilities = defaultCapabilities()
  }
}

async function handleTestModel(modelName: string) {
  testingModel.value = modelName
  testResult.value = null
  testing.value = true

  try {
    testResult.value = await api<TestResult>('/api/models/test', {
      method: 'POST',
      body: JSON.stringify({ model_name: modelName, provider_id: testProviderId.value }),
    })
  } catch (e) {
    testResult.value = {
      success: false,
      message: '请求失败',
      response_text: null,
      duration_ms: null,
      error: e instanceof Error ? e.message : String(e),
    }
  } finally {
    testing.value = false
    testingModel.value = ''
  }
}

async function handleSubmit() {
  if (!form.value.name || !form.value.base_url) {
    message.warning('请填写必填字段')
    return false
  }
  const nameValid = /^[A-Za-z][A-Za-z0-9]*$/.test(form.value.name)
  if (!nameValid) {
    message.error('供应商名称仅支持英文字母与数字，必须以字母开头，不能包含空格或特殊字符')
    return false
  }
  const dup = providers.value.find(
    (p) => p.id !== editingId.value && p.name.toLowerCase() === form.value.name.toLowerCase(),
  )
  if (dup) {
    message.error(`供应商名称 '${form.value.name}' 已存在`)
    return false
  }
  if (!isEditing.value && !form.value.api_key) {
    message.warning('请输入 API Key')
    return false
  }

  modalLoading.value = true
  try {
    if (isEditing.value) {
      const body: Record<string, unknown> = {
        name: form.value.name,
        base_url: form.value.base_url,
        format: form.value.format,
        endpoint_path: form.value.endpoint_path || null,
        upstream_user_agent: form.value.upstream_user_agent || '',
        models: form.value.models.map((m) => ({
          id: m.id,
          model_name: m.model_name,
          target_model: m.target_model ?? null,
          context_window: m.context_window,
          capabilities: m.capabilities,
        })),
      }
      if (form.value.api_key) {
        body.api_key = form.value.api_key
      }
      await api(`/api/providers/${editingId.value}`, {
        method: 'PUT',
        body: JSON.stringify(body),
      })
      message.success('供应商更新成功')
    } else {
      await api('/api/providers', {
        method: 'POST',
        body: JSON.stringify({
          name: form.value.name,
          base_url: form.value.base_url,
          format: form.value.format,
          endpoint_path: form.value.endpoint_path || null,
          upstream_user_agent: form.value.upstream_user_agent || '',
          api_key: form.value.api_key,
          models: form.value.models.map((m) => ({
            model_name: m.model_name,
            target_model: null,
            capabilities: m.capabilities,
          })),
        }),
      })
      message.success('供应商创建成功')
    }
    showModal.value = false
    await fetchProviders()
  } catch (err) {
    message.error(`${isEditing.value ? '更新' : '创建'}失败: ${err}`)
  } finally {
    modalLoading.value = false
  }
  return false
}

async function handleToggle(id: string, enabled: boolean) {
  try {
    await api(`/api/providers/${id}/toggle`, { method: 'PUT' })
    message.success(enabled ? '供应商已启用' : '供应商已禁用')
    await fetchProviders()
  } catch (err) {
    message.error(`操作失败: ${err}`)
  }
}

async function handleDelete(id: string) {
  try {
    await api(`/api/providers/${id}`, { method: 'DELETE' })
    message.success('供应商已删除')
    await fetchProviders()
  } catch (err) {
    message.error(`删除失败: ${err}`)
  }
}

async function fetchProviders() {
  loading.value = true
  try {
    providers.value = await api<Provider[]>('/api/providers')
  } catch (err) {
    console.error('Failed to load providers:', err)
  } finally {
    loading.value = false
  }
}

onMounted(fetchProviders)
</script>

<style scoped>
.provider-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.provider-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(340px, 1fr));
  gap: 16px;
}

.provider-card {
  position: relative;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 16px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  transition: box-shadow 0.2s, transform 0.15s;
  overflow: hidden;
}

.provider-card:hover {
  box-shadow: var(--shadow-md);
  transform: translateY(-1px);
}

.provider-card--disabled {
  opacity: 0.5;
}

.provider-card-bar {
  position: absolute;
  top: 0;
  left: 0;
  right: 0;
  height: 3px;
  background: var(--accent);
}

.provider-card[data-format="completions"] .provider-card-bar {
  background: var(--success);
}
.provider-card[data-format="responses"] .provider-card-bar {
  background: var(--info);
}
.provider-card[data-format="anthropic"] .provider-card-bar {
  background: var(--warning);
}
.provider-card[data-format="gemini"] .provider-card-bar {
  background: #a855f7;
}

.provider-card-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.provider-card-title {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}

.provider-card-name {
  font-weight: 600;
  font-size: 15px;
  color: var(--text-1);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.provider-card-url {
  font-size: 12px;
  word-break: break-all;
  line-height: 1.4;
}

.provider-card-models {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
  min-height: 24px;
}

.provider-model-tag {
  font-size: 11px;
  background: var(--bg-sunken) !important;
}

.provider-card-actions {
  display: flex;
  gap: 2px;
  padding-top: 4px;
  border-top: 1px solid var(--border);
  margin-top: auto;
}
</style>
