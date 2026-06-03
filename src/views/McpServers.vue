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
            <n-button @click="openJsonViewAll">
              JSON 视图
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
          :scroll-x="900"
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
            <n-checkbox value="codex" label="Codex" />
          </n-space>
        </n-checkbox-group>
      </n-space>
    </n-modal>

    <!-- Apply Row Modal -->
    <n-modal
      v-model:show="showApplyRowModal"
      preset="dialog"
      :title="`应用 MCP - ${applyRowServer?.name || ''}`"
      positive-text="应用"
      negative-text="取消"
      :loading="applyRowLoading"
      @positive-click="handleApplyRow"
      style="width: 420px"
    >
      <n-space vertical>
        <n-text>选择要注入到哪些应用：</n-text>
        <n-checkbox-group v-model:value="applyRowTargets">
          <n-space vertical>
            <n-checkbox value="claude_cli" label="Claude CLI" />
            <n-checkbox value="claude_desktop" label="Claude Desktop" />
            <n-checkbox value="codex" label="Codex" />
          </n-space>
        </n-checkbox-group>
      </n-space>
    </n-modal>

    <!-- JSON Editor Modal -->
    <n-modal
      v-model:show="showJsonModal"
      :style="jsonEditorFullscreen ? { width: '100vw', top: 0, left: 0, margin: 0 } : { width: '780px' }"
    >
      <n-card
        title="JSON 配置编辑器"
        :bordered="false"
        size="small"
        role="dialog"
        aria-modal="true"
      >
        <template #header-extra>
          <n-button quaternary size="small" @click="jsonEditorFullscreen = !jsonEditorFullscreen">
            <template #icon>
              <n-icon><contract-outline v-if="jsonEditorFullscreen" /><expand-outline v-else /></n-icon>
            </template>
          </n-button>
        </template>
        <div :style="{
          height: jsonEditorFullscreen ? 'calc(100vh - 160px)' : '480px',
          overflow: 'hidden',
          border: '1px solid #313244',
          borderRadius: '4px'
        }">
          <codemirror
            v-model="jsonViewContent"
            :style="{ height: '100%' }"
            :extensions="jsonEditorExtensions"
          />
        </div>
        <template #action>
          <n-space justify="end">
            <n-button @click="showJsonModal = false">取消</n-button>
            <n-button type="primary" :loading="jsonSaveLoading" @click="handleJsonSave">保存</n-button>
          </n-space>
        </template>
      </n-card>
    </n-modal>
  </n-space>
</template>

<script setup lang="ts">
import { ref, h, onMounted, computed } from 'vue'
import { NTag, NPopconfirm, NButton, NSpace, NTooltip, NIcon, useMessage } from 'naive-ui'
import { DownloadOutline, ExpandOutline, ContractOutline } from '@vicons/ionicons5'
import { Codemirror } from 'vue-codemirror'
import { basicSetup } from 'codemirror'
import { json } from '@codemirror/lang-json'
import { oneDark } from '@codemirror/theme-one-dark'
import { api } from '../api'
import type { McpServerWithBindings, McpAppBindingInput, CreateMcpServerBody } from '../types/mcp'
import type { AppType } from '../types'

interface EnvPair {
  key: string
  value: string
}

const message = useMessage()
const loading = ref(false)

const jsonEditorExtensions = computed(() => [basicSetup, json(), oneDark])
const servers = ref<McpServerWithBindings[]>([])
const showModal = ref(false)
const isEditing = ref(false)
const editingId = ref('')
const modalLoading = ref(false)
const showApplyModal = ref(false)
const applyLoading = ref(false)
const applyTargets = ref<string[]>([])
const showApplyRowModal = ref(false)
const applyRowLoading = ref(false)
const applyRowTargets = ref<string[]>([])
const applyRowServer = ref<McpServerWithBindings | null>(null)
const showJsonModal = ref(false)
const jsonViewContent = ref('')
const jsonSaveLoading = ref(false)
const jsonEditorFullscreen = ref(false)

const form = ref({
  name: '',
  transport_type: 'stdio' as string,
  command: '',
  args: '',
  url: '',
  headers: '',
  envPairs: [] as EnvPair[],
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
  codex: 'Codex',
}

const importOptions = [
  { label: 'Claude CLI', key: 'claude_cli' },
  { label: 'Claude Desktop', key: 'claude_desktop' },
  { label: 'Codex', key: 'codex' },
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
      const enabledBindings = row.bindings.filter((b) => b.enabled)
      const hasCodexCli = enabledBindings.some((b) => b.app_type === 'codex_cli')
      const hasCodexDesktop = enabledBindings.some((b) => b.app_type === 'codex_desktop')
      const displayNames: string[] = []
      for (const b of enabledBindings) {
        if ((b.app_type === 'codex_cli' || b.app_type === 'codex_desktop') && hasCodexCli && hasCodexDesktop) {
          if (b.app_type === 'codex_cli') displayNames.push('Codex')
          continue
        }
        const name = appNameMap[b.app_type] || b.app_type
        if (!displayNames.includes(name)) displayNames.push(name)
      }
      if (displayNames.length === 0) {
        return h(NTag, { size: 'small', type: 'default' as never }, () => '未绑定')
      }
      return h(NSpace, { size: 4 }, () =>
        displayNames.map((name) =>
          h(NTag, { size: 'small', type: 'success' as never }, () => name)
        )
      )
    },
  },
  {
    title: '操作',
    key: 'actions',
    width: 200,
    fixed: 'right',
    render: (row: McpServerWithBindings) =>
      h(NSpace, { size: 4 }, () => [
        h(
          NButton,
          { size: 'small', quaternary: true, type: 'primary', onClick: () => openEditModal(row) },
          () => '编辑'
        ),
        h(
          NButton,
          { size: 'small', quaternary: true, type: 'info', onClick: () => openApplyRowModal(row) },
          () => '应用'
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

function openJsonViewAll() {
  const all: Record<string, Record<string, unknown>> = {}
  for (const row of servers.value) {
    const config: Record<string, unknown> = {}
    if (row.transport_type === 'stdio') {
      config.type = 'stdio'
      if (row.command) config.command = row.command
      if (row.args) {
        try { config.args = JSON.parse(row.args) } catch { config.args = row.args }
      }
    } else {
      config.type = row.transport_type
      if (row.url) config.url = row.url
      if (row.headers) {
        try { config.headers = JSON.parse(row.headers) } catch { config.headers = row.headers }
      }
    }
    if (row.env) {
      try { config.env = JSON.parse(row.env) } catch { config.env = row.env }
    }
    all[row.name] = config
  }
  jsonViewContent.value = JSON.stringify({ mcpServers: all }, null, 2)
  jsonEditorFullscreen.value = false
  showJsonModal.value = true
}

async function handleJsonSave() {
  let parsed: { mcpServers: Record<string, Record<string, unknown>> }
  try {
    parsed = JSON.parse(jsonViewContent.value)
  } catch {
    message.error('JSON 格式错误')
    return false
  }

  const entries = parsed.mcpServers
  if (!entries || typeof entries !== 'object') {
    message.error('缺少 mcpServers 字段')
    return false
  }

  jsonSaveLoading.value = true
  try {
    const existingMap = new Map(servers.value.map((s) => [s.name, s]))
    const newNames = new Set(Object.keys(entries))

    for (const name of newNames) {
      const entry = entries[name]
      const body: Record<string, unknown> = {
        name,
        transport_type: (entry.type as string) || 'stdio',
      }
      if (body.transport_type === 'stdio') {
        if (entry.command) body.command = entry.command
        if (entry.args) body.args = Array.isArray(entry.args) ? JSON.stringify(entry.args) : entry.args
      } else {
        if (entry.url) body.url = entry.url
        if (entry.headers && typeof entry.headers === 'object') body.headers = JSON.stringify(entry.headers)
      }
      if (entry.env && typeof entry.env === 'object') body.env = JSON.stringify(entry.env)

      const existing = existingMap.get(name)
      if (existing) {
        await api(`/api/mcp/servers/${existing.id}`, {
          method: 'PUT',
          body: JSON.stringify(body),
        })
      } else {
        await api('/api/mcp/servers', {
          method: 'POST',
          body: JSON.stringify(body),
        })
      }
    }

    for (const [name, server] of existingMap) {
      if (!newNames.has(name)) {
        await api(`/api/mcp/servers/${server.id}`, { method: 'DELETE' })
      }
    }

    message.success('JSON 配置已保存')
    showJsonModal.value = false
    await fetchServers()
  } catch (err) {
    message.error(`保存失败: ${err}`)
  } finally {
    jsonSaveLoading.value = false
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

function openApplyRowModal(row: McpServerWithBindings) {
  applyRowServer.value = row
  const enabledTypes = row.bindings
    .filter((b) => b.enabled)
    .map((b) => b.app_type)
  const targets: string[] = []
  for (const t of enabledTypes) {
    if (t === 'codex_cli' || t === 'codex_desktop') {
      if (!targets.includes('codex')) targets.push('codex')
    } else {
      targets.push(t)
    }
  }
  applyRowTargets.value = targets
  showApplyRowModal.value = true
}

async function handleApplyRow() {
  if (applyRowTargets.value.length === 0 && !applyRowServer.value) {
    message.warning('请至少选择一个应用')
    return false
  }

  const server = applyRowServer.value!
  applyRowLoading.value = true
  try {
    const allApps = ['claude_cli', 'claude_desktop', 'codex_cli', 'codex_desktop']
    const selectedApps = applyRowTargets.value.flatMap((t) =>
      t === 'codex' ? ['codex_cli', 'codex_desktop'] : [t]
    )
    const bindings: McpAppBindingInput[] = allApps.map((appType) => ({
      app_type: appType as AppType,
      enabled: selectedApps.includes(appType),
    }))

    await api(`/api/mcp/servers/${server.id}/bindings`, {
      method: 'PUT',
      body: JSON.stringify({ bindings }),
    })

    const appsToApply = new Set<string>()
    for (const app of selectedApps) {
      if (app === 'codex_cli' || app === 'codex_desktop') {
        appsToApply.add('codex')
      } else {
        appsToApply.add(app)
      }
    }
    for (const appType of appsToApply) {
      await api(`/api/mcp/apply/${appType}`, { method: 'POST' })
    }

    message.success('配置已应用')
    showApplyRowModal.value = false
    await fetchServers()
  } catch (err) {
    message.error(`应用失败: ${err}`)
  } finally {
    applyRowLoading.value = false
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
