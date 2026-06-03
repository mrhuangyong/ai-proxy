<template>
  <n-space vertical size="large">
    <n-card>
      <template #header>
        <n-space justify="space-between" align="center">
          <n-text strong>MCP 管理</n-text>
          <n-space>
            <n-dropdown :options="importOptions" @select="handleImport">
              <n-button>
                从程序导入
                <template #icon>
                  <n-icon><download-outline /></n-icon>
                </template>
              </n-button>
            </n-dropdown>
            <n-button @click="openApplyModal">
              应用配置
            </n-button>
            <n-button type="primary" @click="openCreateModal">
              添加 MCP
            </n-button>
          </n-space>
        </n-space>
      </template>

      <n-spin :show="loading">
        <n-data-table
          :columns="columns"
          :data="servers"
          :row-key="(row: McpServerWithBindings) => row.id"
          :bordered="false"
        />
      </n-spin>
    </n-card>

    <!-- Add/Edit Modal -->
    <n-modal
      v-model:show="showModal"
      preset="dialog"
      :title="isEditing ? '编辑 MCP' : '添加 MCP'"
      positive-text="确认"
      negative-text="取消"
      :loading="modalLoading"
      @positive-click="handleSubmit"
      style="width: 640px"
    >
      <n-form :model="form" label-placement="left" label-width="100">
        <n-form-item label="名称" required>
          <n-input v-model:value="form.name" placeholder="例如: filesystem" :input-props="{ autocapitalize: 'off' }" />
        </n-form-item>

        <n-form-item label="传输类型">
          <n-select
            v-model:value="form.transport_type"
            :options="transportTypeOptions"
            placeholder="选择传输类型"
          />
        </n-form-item>

        <template v-if="form.transport_type === 'stdio'">
          <n-form-item label="Command" required>
            <n-input v-model:value="form.command" placeholder="例如: npx" :input-props="{ autocapitalize: 'off' }" />
          </n-form-item>
          <n-form-item label="Args">
            <n-input
              v-model:value="form.args"
              type="textarea"
              placeholder="每行一个参数，例如:&#10;-y&#10;@modelcontextprotocol/server-filesystem&#10;/tmp"
              :rows="3"
              :input-props="{ autocapitalize: 'off' }"
            />
          </n-form-item>
        </template>

        <template v-else>
          <n-form-item label="URL" required>
            <n-input v-model:value="form.url" placeholder="例如: http://localhost:3000/sse" :input-props="{ autocapitalize: 'off' }" />
          </n-form-item>
          <n-form-item label="Headers">
            <n-input
              v-model:value="form.headers"
              type="textarea"
              placeholder='JSON 格式，例如: {"Authorization": "Bearer xxx"}'
              :rows="2"
              :input-props="{ autocapitalize: 'off' }"
            />
          </n-form-item>
        </template>

        <n-divider>环境变量</n-divider>

        <n-space vertical size="small">
          <div
            v-for="(_, index) in form.envPairs"
            :key="index"
            style="display: flex; align-items: center; gap: 8px"
          >
            <n-input
              v-model:value="form.envPairs[index].key"
              placeholder="变量名"
              style="flex: 1"
              :input-props="{ autocapitalize: 'off' }"
            />
            <n-input
              v-model:value="form.envPairs[index].value"
              placeholder="变量值"
              style="flex: 1"
              :input-props="{ autocapitalize: 'off' }"
            />
            <n-button
              quaternary
              type="error"
              size="small"
              @click="removeEnvPair(index)"
            >
              删除
            </n-button>
          </div>
          <n-button dashed size="small" @click="addEnvPair">
            + 添加环境变量
          </n-button>
        </n-space>

        <n-divider>应用绑定</n-divider>

        <n-checkbox-group v-model:value="form.bindings">
          <n-space>
            <n-checkbox value="claude_cli" label="Claude CLI" />
            <n-checkbox value="claude_desktop" label="Claude Desktop" />
          </n-space>
        </n-checkbox-group>
      </n-form>
    </n-modal>

    <!-- Apply Modal -->
    <n-modal
      v-model:show="showApplyModal"
      preset="dialog"
      title="应用配置"
      positive-text="应用"
      negative-text="取消"
      :loading="applyLoading"
      @positive-click="handleApply"
      style="width: 420px"
    >
      <n-space vertical>
        <n-text>选择要应用到哪些程序：</n-text>
        <n-checkbox-group v-model:value="applyTargets">
          <n-space vertical>
            <n-checkbox value="claude_cli" label="Claude CLI" />
            <n-checkbox value="claude_desktop" label="Claude Desktop" />
          </n-space>
        </n-checkbox-group>
      </n-space>
    </n-modal>
  </n-space>
</template>

<script setup lang="ts">
import { ref, h, onMounted } from 'vue'
import { NTag, NPopconfirm, NButton, NSpace, NTooltip, useMessage } from 'naive-ui'
import { DownloadOutline } from '@vicons/ionicons5'
import { api } from '../api'
import type { McpServerWithBindings, McpAppBindingInput, CreateMcpServerBody } from '../types/mcp'
import type { AppType } from '../types'

interface EnvPair {
  key: string
  value: string
}

const message = useMessage()
const loading = ref(false)
const servers = ref<McpServerWithBindings[]>([])
const showModal = ref(false)
const isEditing = ref(false)
const editingId = ref('')
const modalLoading = ref(false)
const showApplyModal = ref(false)
const applyLoading = ref(false)
const applyTargets = ref<string[]>([])

const form = ref({
  name: '',
  transport_type: 'stdio' as string,
  command: '',
  args: '',
  url: '',
  headers: '',
  envPairs: [] as EnvPair[],
  bindings: [] as string[],
})

const transportTypeOptions = [
  { label: 'stdio', value: 'stdio' },
  { label: 'SSE', value: 'sse' },
  { label: 'Streamable HTTP', value: 'streamable-http' },
]

const transportColorMap: Record<string, string> = {
  stdio: 'success',
  sse: 'info',
  'streamable-http': 'warning',
}

const appNameMap: Record<string, string> = {
  claude_cli: 'Claude CLI',
  claude_desktop: 'Claude Desktop',
}

const importOptions = [
  { label: 'Claude CLI', key: 'claude_cli' },
  { label: 'Claude Desktop', key: 'claude_desktop' },
]

const columns = [
  {
    title: '名称',
    key: 'name',
    width: 160,
    render: (row: McpServerWithBindings) =>
      h(NTooltip, {}, {
        trigger: () => h('span', {}, row.name),
        default: () => row.description || row.name,
      }),
  },
  {
    title: '传输类型',
    key: 'transport_type',
    width: 120,
    render: (row: McpServerWithBindings) => {
      const color = transportColorMap[row.transport_type] || 'default'
      return h(NTag, { size: 'small', type: color as never }, () => row.transport_type)
    },
  },
  {
    title: '命令/URL',
    key: 'command_or_url',
    ellipsis: { tooltip: true },
    render: (row: McpServerWithBindings) => {
      if (row.transport_type === 'stdio') {
        const parts = [row.command, row.args].filter(Boolean)
        return parts.join(' ')
      }
      return row.url || '-'
    },
  },
  {
    title: '绑定应用',
    key: 'bindings',
    width: 200,
    render: (row: McpServerWithBindings) => {
      const boundApps = row.bindings
        .filter((b) => b.enabled)
        .map((b) => appNameMap[b.app_type] || b.app_type)
      if (boundApps.length === 0) {
        return h(NTag, { size: 'small', type: 'default' as never }, () => '未绑定')
      }
      return h(NSpace, { size: 4 }, () =>
        boundApps.map((name) =>
          h(NTag, { size: 'small', type: 'success' as never }, () => name)
        )
      )
    },
  },
  {
    title: '操作',
    key: 'actions',
    width: 240,
    render: (row: McpServerWithBindings) =>
      h(NSpace, { size: 4 }, () => [
        h(
          NButton,
          { size: 'small', quaternary: true, type: 'primary', onClick: () => openEditModal(row) },
          () => '编辑'
        ),
        h(
          NPopconfirm,
          { onPositiveClick: () => handleDelete(row.id) },
          {
            trigger: () =>
              h(NButton, { size: 'small', quaternary: true, type: 'error' }, () => '删除'),
            default: () => '确认删除此 MCP 服务？',
          }
        ),
      ]),
  },
]

function addEnvPair() {
  form.value.envPairs = [...form.value.envPairs, { key: '', value: '' }]
}

function removeEnvPair(index: number) {
  form.value.envPairs = form.value.envPairs.filter((_, i) => i !== index)
}

function parseArgsToText(argsJson: string | null): string {
  if (!argsJson) return ''
  try {
    const arr = JSON.parse(argsJson) as string[]
    return arr.join('\n')
  } catch {
    return argsJson
  }
}

function buildArgsJson(): string | null {
  const lines = form.value.args
    .split('\n')
    .map(l => l.trim())
    .filter(l => l.length > 0)
  return lines.length > 0 ? JSON.stringify(lines) : null
}

function parseEnvFromJson(envJson: string | null): EnvPair[] {
  if (!envJson) return []
  try {
    const parsed = JSON.parse(envJson) as Record<string, string>
    return Object.entries(parsed).map(([key, value]) => ({ key, value }))
  } catch {
    return []
  }
}

function buildEnvJson(): string | null {
  const pairs = form.value.envPairs.filter((p) => p.key.trim())
  if (pairs.length === 0) return null
  const obj: Record<string, string> = {}
  for (const p of pairs) {
    obj[p.key.trim()] = p.value
  }
  return JSON.stringify(obj)
}

function openCreateModal() {
  isEditing.value = false
  editingId.value = ''
  form.value = {
    name: '',
    transport_type: 'stdio',
    command: '',
    args: '',
    url: '',
    headers: '',
    envPairs: [],
    bindings: [],
  }
  showModal.value = true
}

function openEditModal(row: McpServerWithBindings) {
  isEditing.value = true
  editingId.value = row.id
  form.value = {
    name: row.name,
    transport_type: row.transport_type,
    command: row.command || '',
    args: parseArgsToText(row.args),
    url: row.url || '',
    headers: row.headers || '',
    envPairs: parseEnvFromJson(row.env),
    bindings: row.bindings
      .filter((b) => b.enabled)
      .map((b) => b.app_type),
  }
  showModal.value = true
}

function buildHeadersJson(): string | null {
  const raw = form.value.headers.trim()
  if (!raw) return null
  try {
    JSON.parse(raw)
    return raw
  } catch {
    return null
  }
}

async function handleSubmit() {
  if (!form.value.name.trim()) {
    message.warning('请填写名称')
    return false
  }
  if (form.value.transport_type === 'stdio' && !form.value.command.trim()) {
    message.warning('请填写 Command')
    return false
  }
  if (form.value.transport_type !== 'stdio' && !form.value.url.trim()) {
    message.warning('请填写 URL')
    return false
  }

  modalLoading.value = true
  try {
    const envJson = buildEnvJson()
    const headersJson = form.value.transport_type !== 'stdio' ? buildHeadersJson() : null
    const bindings: McpAppBindingInput[] = form.value.bindings.map((appType) => ({
      app_type: appType as AppType,
      enabled: true,
    }))

    if (isEditing.value) {
      const updateBody: Record<string, unknown> = {
        name: form.value.name.trim(),
        transport_type: form.value.transport_type,
      }
      if (form.value.transport_type === 'stdio') {
        updateBody.command = form.value.command.trim()
        updateBody.args = buildArgsJson()
        updateBody.url = null
        updateBody.headers = null
      } else {
        updateBody.url = form.value.url.trim()
        updateBody.headers = headersJson
        updateBody.command = null
        updateBody.args = null
      }
      updateBody.env = envJson

      await api(`/api/mcp/servers/${editingId.value}`, {
        method: 'PUT',
        body: JSON.stringify(updateBody),
      })

      await api(`/api/mcp/servers/${editingId.value}/bindings`, {
        method: 'PUT',
        body: JSON.stringify({ bindings }),
      })

      message.success('MCP 更新成功')
    } else {
      const createBody: CreateMcpServerBody = {
        name: form.value.name.trim(),
        transport_type: form.value.transport_type,
      }
      if (form.value.transport_type === 'stdio') {
        createBody.command = form.value.command.trim()
        createBody.args = buildArgsJson()
      } else {
        createBody.url = form.value.url.trim()
        createBody.headers = headersJson
      }
      createBody.env = envJson
      createBody.bindings = bindings

      await api('/api/mcp/servers', {
        method: 'POST',
        body: JSON.stringify(createBody),
      })

      message.success('MCP 创建成功')
    }

    showModal.value = false
    await fetchServers()
  } catch (err) {
    message.error(`${isEditing.value ? '更新' : '创建'}失败: ${err}`)
  } finally {
    modalLoading.value = false
  }
  return false
}

async function handleDelete(id: string) {
  try {
    await api(`/api/mcp/servers/${id}`, { method: 'DELETE' })
    message.success('MCP 已删除')
    await fetchServers()
  } catch (err) {
    message.error(`删除失败: ${err}`)
  }
}

function openApplyModal() {
  applyTargets.value = []
  showApplyModal.value = true
}

async function handleApply() {
  if (applyTargets.value.length === 0) {
    message.warning('请至少选择一个应用')
    return false
  }

  applyLoading.value = true
  try {
    for (const appType of applyTargets.value) {
      await api(`/api/mcp/apply/${appType}`, { method: 'POST' })
    }
    message.success('配置已应用')
    showApplyModal.value = false
    await fetchServers()
  } catch (err) {
    message.error(`应用失败: ${err}`)
  } finally {
    applyLoading.value = false
  }
  return false
}

async function handleImport(appType: string) {
  try {
    const result = await api<{ imported: number; skipped: number }>(`/api/mcp/import/${appType}`, {
      method: 'POST',
    })
    message.success(`导入完成：成功 ${result.imported} 个，跳过 ${result.skipped} 个`)
    await fetchServers()
  } catch (err) {
    message.error(`导入失败: ${err}`)
  }
}

async function fetchServers() {
  loading.value = true
  try {
    servers.value = await api<McpServerWithBindings[]>('/api/mcp/servers')
  } catch (err) {
    console.error('Failed to load MCP servers:', err)
  } finally {
    loading.value = false
  }
}

onMounted(fetchServers)
</script>
