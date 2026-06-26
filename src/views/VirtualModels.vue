<template>
  <n-space vertical size="large">
    <div class="vm-header">
      <n-text strong style="font-size: 16px">虚拟模型</n-text>
      <n-space>
        <n-button size="small" quaternary @click="fetchData">刷新</n-button>
        <n-button type="primary" size="small" @click="openCreateModal">新建虚拟模型</n-button>
      </n-space>
    </div>

    <n-spin :show="loading">
      <div class="vm-layout">
        <!-- 左：虚拟模型列表 -->
        <div class="vm-left">
          <n-input v-model:value="filter" placeholder="搜索虚拟模型" clearable size="small" style="margin-bottom: 8px" />
          <div class="vm-list">
            <div
              v-for="v in filtered"
              :key="v.id"
              class="vm-item"
              :class="{ active: selected?.id === v.id, disabled: !v.enabled }"
              @click="selectVirtual(v)"
            >
              <div class="vm-item-top">
                <span class="vm-item-name">{{ v.name }}</span>
                <n-tag size="tiny" :type="v.enabled ? 'success' : 'default'">
                  {{ v.enabled ? '启用' : '停用' }}
                </n-tag>
              </div>
              <div class="vm-item-meta">
                <n-tag size="tiny" :type="healthyType(v)">
                  可用 {{ availableCount(v) }}/{{ v.mappings.length }}
                </n-tag>
                <div class="vm-item-actions" @click.stop>
                  <n-button quaternary size="tiny" @click="openEditModal(v)">编辑</n-button>
                  <n-popconfirm @positive-click="delVirtual(v.id)">
                    <template #trigger>
                      <n-button quaternary size="tiny" type="error">删</n-button>
                    </template>
                    确认删除虚拟模型「{{ v.name }}」？
                  </n-popconfirm>
                </div>
              </div>
            </div>
            <n-empty v-if="filtered.length === 0" description="暂无虚拟模型" style="padding: 32px 0" />
          </div>
        </div>

        <!-- 右：真实模型映射 -->
        <div class="vm-right">
          <template v-if="selected">
            <div class="vm-right-header">
              <div class="vm-right-title">
                <n-text strong style="font-size: 15px">{{ selected.name }}</n-text>
                <n-text v-if="selected.description" depth="3" style="font-size: 12px; margin-left: 8px">
                  {{ selected.description }}
                </n-text>
              </div>
              <n-space>
                <n-switch
                  :value="selected.enabled"
                  size="small"
                  @update:value="(v: boolean) => toggleVirtualEnabled(v)"
                />
                <n-button size="small" type="primary" @click="openAddMappingModal">+ 挂载真实模型</n-button>
              </n-space>
            </div>

            <n-data-table
              :columns="mappingColumns"
              :data="selected.mappings"
              :bordered="false"
              :row-key="(row: VirtualModelMapping) => row.id"
              size="small"
            />
          </template>
          <n-empty v-else description="选择左侧虚拟模型查看映射" style="padding: 64px 0" />
        </div>
      </div>
    </n-spin>

    <!-- 虚拟模型 新建/编辑 modal -->
    <n-modal
      v-model:show="showModal"
      preset="dialog"
      :title="isEditing ? '编辑虚拟模型' : '新建虚拟模型'"
      positive-text="确认"
      negative-text="取消"
      :loading="modalLoading"
      @positive-click="handleSubmit"
      style="width: 560px"
    >
      <n-form :model="form" label-placement="left" label-width="90">
        <n-form-item label="名称" required>
          <n-input v-model:value="form.name" placeholder="例如 glm-5.2" :input-props="{ autocapitalize: 'off' }" />
        </n-form-item>
        <n-form-item label="描述">
          <n-input v-model:value="form.description" placeholder="可选" />
        </n-form-item>
        <n-form-item label="启用">
          <n-switch v-model:value="form.enabled" />
        </n-form-item>
        <template v-if="!isEditing">
          <n-divider style="margin: 12px 0">初始挂载（可选，创建后可继续添加）</n-divider>
          <div v-for="(_, idx) in form.initMappings" :key="idx" class="vm-form-row">
            <n-select
              v-model:value="form.initMappings[idx].provider_model_id"
              :options="realModelOptions"
              filterable
              placeholder="选择真实模型"
              style="flex: 1"
            />
            <n-input-number v-model:value="form.initMappings[idx].priority" :min="0" placeholder="优先级" style="width: 120px" />
            <n-button quaternary type="error" size="small" @click="form.initMappings.splice(idx, 1)">删</n-button>
          </div>
          <n-button dashed size="small" @click="addInitMapping">+ 添加映射</n-button>
        </template>
      </n-form>
    </n-modal>

    <!-- 挂载真实模型 modal -->
    <n-modal
      v-model:show="showAddMappingModal"
      preset="dialog"
      title="挂载真实模型"
      positive-text="确认"
      negative-text="取消"
      :loading="addMappingLoading"
      @positive-click="handleAddMapping"
      style="width: 480px"
    >
      <n-form label-placement="left" label-width="90">
        <n-form-item label="真实模型" required>
          <n-select
            v-model:value="newMapping.provider_model_id"
            :options="realModelOptions"
            filterable
            placeholder="provider_name/model_name"
          />
        </n-form-item>
        <n-form-item label="优先级">
          <n-input-number v-model:value="newMapping.priority" :min="0" />
        </n-form-item>
        <n-form-item label="启用">
          <n-switch v-model:value="newMapping.enabled" />
        </n-form-item>
      </n-form>
    </n-modal>
  </n-space>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, h } from 'vue'
import { api } from '../api'
import {
  NTag,
  NPopconfirm,
  NButton,
  NSwitch,
  NSpace,
  NInputNumber,
  useMessage,
} from 'naive-ui'
import type {
  VirtualModel,
  VirtualModelMapping,
  RealModelOption,
  CreateMappingInput,
  SetAvailableBody,
  SetStickyBody,
} from '../types'

const message = useMessage()
const loading = ref(false)
const virtualModels = ref<VirtualModel[]>([])
const selectedId = ref<string | null>(null)
const selected = computed(() => virtualModels.value.find((v) => v.id === selectedId.value) ?? null)
const filter = ref('')
const realModelOptions = ref<{ label: string; value: string }[]>([])

const filtered = computed(() =>
  virtualModels.value.filter((v) => v.name.toLowerCase().includes(filter.value.toLowerCase())),
)

// --- 弹窗状态 ---
const showModal = ref(false)
const isEditing = ref(false)
const editingId = ref('')
const modalLoading = ref(false)
const form = ref<{ name: string; description: string; enabled: boolean; initMappings: CreateMappingInput[] }>({
  name: '',
  description: '',
  enabled: true,
  initMappings: [],
})

const showAddMappingModal = ref(false)
const addMappingLoading = ref(false)
const newMapping = ref<{ provider_model_id: string; priority: number; enabled: boolean }>({
  provider_model_id: '',
  priority: 100,
  enabled: true,
})

// --- 初始化 ---
onMounted(async () => {
  await fetchData()
  await fetchRealModels()
})

async function fetchData() {
  loading.value = true
  try {
    const list = await api<VirtualModel[]>('/api/virtual-models')
    virtualModels.value = list
    // 保持选中
    if (selectedId.value && !list.some((v) => v.id === selectedId.value)) {
      selectedId.value = list[0]?.id ?? null
    } else if (!selectedId.value && list.length > 0) {
      selectedId.value = list[0].id
    }
  } catch (e: any) {
    message.error(`加载失败：${e?.message ?? e}`)
  } finally {
    loading.value = false
  }
}

async function fetchRealModels() {
  try {
    const list = await api<RealModelOption[]>('/api/virtual-models/real-models')
    realModelOptions.value = list.map((r) => ({ label: r.label, value: r.provider_model_id }))
  } catch {
    // 静默；下拉空也允许
  }
}

function selectVirtual(v: VirtualModel) {
  selectedId.value = v.id
}

function availableCount(v: VirtualModel): number {
  return v.mappings.filter((m) => m.available && m.enabled).length
}

function healthyType(v: VirtualModel): 'success' | 'warning' | 'error' | 'default' {
  const total = v.mappings.length
  if (total === 0) return 'default'
  const avail = availableCount(v)
  if (avail === 0) return 'error'
  if (avail < total) return 'warning'
  return 'success'
}

// --- 虚拟模型 CRUD ---

function openCreateModal() {
  isEditing.value = false
  editingId.value = ''
  form.value = { name: '', description: '', enabled: true, initMappings: [] }
  showModal.value = true
}

function openEditModal(v: VirtualModel) {
  isEditing.value = true
  editingId.value = v.id
  form.value = {
    name: v.name,
    description: v.description ?? '',
    enabled: v.enabled,
    initMappings: [],
  }
  showModal.value = true
}

function addInitMapping() {
  form.value.initMappings.push({ provider_model_id: '', priority: 100, enabled: true })
}

async function handleSubmit(): Promise<boolean> {
  if (!form.value.name.trim()) {
    message.warning('请输入虚拟模型名称')
    return false
  }
  modalLoading.value = true
  try {
    const body = JSON.stringify({
      name: form.value.name.trim(),
      description: form.value.description || null,
      enabled: form.value.enabled,
      mappings: isEditing.value ? [] : form.value.initMappings.filter((m) => m.provider_model_id),
    })
    if (isEditing.value) {
      await api(`/api/virtual-models/${editingId.value}`, { method: 'PUT', body })
      message.success('已更新')
    } else {
      await api('/api/virtual-models', { method: 'POST', body })
      message.success('已创建')
    }
    showModal.value = false
    await fetchData()
    return true
  } catch (e: any) {
    message.error(`操作失败：${e?.message ?? e}`)
    return false
  } finally {
    modalLoading.value = false
  }
}

async function toggleVirtualEnabled(v: boolean) {
  if (!selected.value) return
  try {
    await api(`/api/virtual-models/${selected.value.id}`, {
      method: 'PUT',
      body: JSON.stringify({ enabled: v }),
    })
    await fetchData()
  } catch (e: any) {
    message.error(`切换失败：${e?.message ?? e}`)
  }
}

async function delVirtual(id: string) {
  try {
    await api(`/api/virtual-models/${id}`, { method: 'DELETE' })
    if (selectedId.value === id) selectedId.value = null
    await fetchData()
    message.success('已删除')
  } catch (e: any) {
    message.error(`删除失败：${e?.message ?? e}`)
  }
}

// --- 映射 CRUD ---

function openAddMappingModal() {
  newMapping.value = { provider_model_id: '', priority: 100, enabled: true }
  fetchRealModels()
  showAddMappingModal.value = true
}

async function handleAddMapping(): Promise<boolean> {
  if (!selected.value) return false
  if (!newMapping.value.provider_model_id) {
    message.warning('请选择真实模型')
    return false
  }
  addMappingLoading.value = true
  try {
    await api(`/api/virtual-models/${selected.value.id}/mappings`, {
      method: 'POST',
      body: JSON.stringify({
        provider_model_id: newMapping.value.provider_model_id,
        priority: newMapping.value.priority,
        enabled: newMapping.value.enabled,
      }),
    })
    showAddMappingModal.value = false
    await fetchData()
    message.success('已挂载')
    return true
  } catch (e: any) {
    message.error(`挂载失败：${e?.message ?? e}`)
    return false
  } finally {
    addMappingLoading.value = false
  }
}

async function updateMapping(mid: string, patch: { priority?: number; enabled?: boolean }) {
  await api(`/api/virtual-models/mappings/${mid}`, { method: 'PUT', body: JSON.stringify(patch) })
  await fetchData()
}

async function setMappingAvailable(mid: string, available: boolean) {
  const body: SetAvailableBody = { available }
  await api(`/api/virtual-models/mappings/${mid}/available`, { method: 'PUT', body: JSON.stringify(body) })
  await fetchData()
}

async function setSticky(mappingId: string | null) {
  if (!selected.value) return
  const body: SetStickyBody = { mapping_id: mappingId }
  await api(`/api/virtual-models/${selected.value.id}/sticky`, { method: 'PUT', body: JSON.stringify(body) })
  await fetchData()
  message.success(mappingId ? '已设为粘性' : '已解除粘性')
}

async function deleteMapping(mid: string) {
  await api(`/api/virtual-models/mappings/${mid}`, { method: 'DELETE' })
  await fetchData()
  message.success('已删除映射')
}

// --- 表格列 ---

const mappingColumns = computed(() => [
  {
    title: '真实模型',
    key: 'label',
    render: (row: VirtualModelMapping) =>
      h(NSpace, { size: 6, align: 'center', wrapItem: false }, () => [
        row.is_current
          ? h(
              NTag,
              { size: 'tiny', type: 'info', round: true, bordered: false },
              { default: () => '粘性' },
            )
          : null,
        h('span', { class: 'font-mono', style: 'font-size:13px' }, row.label),
        row.is_current
          ? h(
              NButton,
              {
                size: 'tiny',
                quaternary: true,
                type: 'warning',
                onClick: () =>
                  setSticky(null).catch((err) => message.error(String(err))),
              },
              { default: () => '解除粘性' },
            )
          : h(
              NButton,
              {
                size: 'tiny',
                quaternary: true,
                type: 'info',
                disabled: !row.enabled || !row.available,
                onClick: () =>
                  setSticky(row.id).catch((err) => message.error(String(err))),
              },
              { default: () => '设为粘性' },
            ),
      ]),
  },
  {
    title: '优先级',
    key: 'priority',
    width: 110,
    render: (row: VirtualModelMapping) =>
      h(NInputNumber as any, {
        value: row.priority,
        min: 0,
        size: 'small',
        showButton: false,
        style: 'width: 80px',
        onBlur: (e: FocusEvent) => {
          const v = (e.target as HTMLInputElement).value
          const n = parseInt(v, 10)
          if (!Number.isNaN(n) && n !== row.priority) {
            updateMapping(row.id, { priority: n }).catch((err) => message.error(String(err)))
          }
        },
      }),
  },
  {
    title: '启用',
    key: 'enabled',
    width: 80,
    render: (row: VirtualModelMapping) =>
      h(NSwitch, {
        value: row.enabled,
        size: 'small',
        onUpdateValue: (v: boolean) => {
          updateMapping(row.id, { enabled: v }).catch((err) => message.error(String(err)))
        },
      }),
  },
  {
    title: '可用',
    key: 'available',
    width: 120,
    render: (row: VirtualModelMapping) =>
      h(NSpace, { size: 4, align: 'center', wrapItem: false }, () => [
        h(NTag, { size: 'small', type: row.available ? 'success' : 'error' }, { default: () => (row.available ? '可用' : '不可用') }),
        h(
          NButton,
          { size: 'tiny', quaternary: true, type: row.available ? 'error' : 'success', onClick: () => setMappingAvailable(row.id, !row.available) },
          { default: () => (row.available ? '停用' : '恢复') },
        ),
      ]),
  },
  {
    title: '失败/轮换',
    key: 'failstats',
    width: 120,
    render: (row: VirtualModelMapping) =>
      h(NSpace, { size: 4, align: 'center', wrapItem: false }, () => [
        h('span', { class: 'tabular-nums', style: 'font-size: 12px' }, `${row.consecutive_failures}次`),
        h('span', { style: 'color: var(--text-3); font-size: 12px' }, '/'),
        h('span', { class: 'tabular-nums', style: 'font-size: 12px' },
          `${row.failover_count}轮`),
      ]),
  },
  {
    title: '操作',
    key: 'actions',
    width: 80,
    render: (row: VirtualModelMapping) =>
      h(
        NPopconfirm,
        { onPositiveClick: () => deleteMapping(row.id) },
        {
          trigger: () => h(NButton, { size: 'tiny', quaternary: true, type: 'error' }, { default: () => '删除' }),
          default: () => `确认删除映射 ${row.label}？`,
        },
      ),
  },
])
</script>

<style scoped>
.vm-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}
.vm-layout {
  display: grid;
  grid-template-columns: 280px 1fr;
  gap: 16px;
  min-height: 560px;
}
@media (max-width: 900px) {
  .vm-layout {
    grid-template-columns: 1fr;
  }
}
.vm-left {
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  padding: 12px;
  height: 70vh;
  overflow-y: auto;
}
.vm-list {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.vm-item {
  padding: 10px 12px;
  border-radius: var(--radius-md);
  border: 1px solid transparent;
  cursor: pointer;
  transition: all 0.12s ease;
}
.vm-item:hover {
  background: var(--bg-sunken);
}
.vm-item.active {
  background: var(--accent-subtle);
  border-color: var(--accent);
}
.vm-item.disabled .vm-item-name {
  opacity: 0.5;
}
.vm-item-top {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 6px;
}
.vm-item-name {
  font-weight: 600;
  font-size: 13px;
}
.vm-item-meta {
  display: flex;
  justify-content: space-between;
  align-items: center;
}
.vm-item-actions {
  display: flex;
  gap: 2px;
}
.vm-right {
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  padding: 16px;
  min-height: 70vh;
}
.vm-right-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 16px;
}
.vm-right-title {
  display: flex;
  align-items: baseline;
}
.vm-form-row {
  display: flex;
  gap: 8px;
  margin-bottom: 8px;
  align-items: center;
}
</style>