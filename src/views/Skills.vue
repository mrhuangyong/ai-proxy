<template>
  <n-space vertical size="large">
    <n-card>
      <template #header>
        <n-space justify="space-between" align="center">
          <n-text strong>技能管理</n-text>
          <n-space>
            <n-button @click="handleScan" :loading="scanLoading">
              刷新扫描
            </n-button>
            <n-button @click="handleAutoDiscover" :loading="discoverLoading">
              自动发现
            </n-button>
            <n-button @click="openAddSourceModal">
              添加源
            </n-button>
            <n-dropdown :options="installOptions" @select="handleInstallSelect">
              <n-button>
                安装技能
              </n-button>
            </n-dropdown>
            <n-button type="primary" @click="openCreateSkillModal">
              创建技能
            </n-button>
          </n-space>
        </n-space>
      </template>

      <n-spin :show="loading">
        <n-tabs
          v-if="sources.length > 0"
          type="line"
          :value="activeTab"
          @update:value="handleTabChange"
        >
          <n-tab-pane
            v-for="source in sources"
            :key="source.id"
            :name="source.id"
            :tab="`${source.name} (${source.skill_count})`"
          >
            <n-data-table
              :columns="columns"
              :data="getSkillsForSource(source.id)"
              :row-key="(row: Skill) => row.id"
              :bordered="false"
              :scroll-x="900"
            />
          </n-tab-pane>
        </n-tabs>
        <n-empty v-else description="暂无技能源，请添加源或自动发现" />
      </n-spin>
    </n-card>

    <!-- Add Source Modal -->
    <n-modal
      v-model:show="showAddSourceModal"
      preset="dialog"
      title="添加技能源"
      positive-text="确认"
      negative-text="取消"
      :loading="addSourceLoading"
      @positive-click="handleAddSource"
      style="width: 480px"
    >
      <n-form :model="addSourceForm" label-placement="left" label-width="80">
        <n-form-item label="名称" required>
          <n-input v-model:value="addSourceForm.name" placeholder="例如: my-skills" :input-props="{ autocapitalize: 'off' }" />
        </n-form-item>
        <n-form-item label="路径" required>
          <n-input v-model:value="addSourceForm.path" placeholder="例如: ~/.claude/skills" :input-props="{ autocapitalize: 'off' }" />
        </n-form-item>
      </n-form>
    </n-modal>

    <!-- Create Skill Modal -->
    <n-modal
      v-model:show="showCreateSkillModal"
      preset="dialog"
      title="创建技能"
      positive-text="确认"
      negative-text="取消"
      :loading="createSkillLoading"
      @positive-click="handleCreateSkill"
      style="width: 560px"
    >
      <n-form :model="createSkillForm" label-placement="left" label-width="100">
        <n-form-item label="目标源" required>
          <n-select
            v-model:value="createSkillForm.sourceId"
            :options="sourceSelectOptions"
            placeholder="选择目标源"
          />
        </n-form-item>
        <n-form-item label="技能名称" required>
          <n-input v-model:value="createSkillForm.name" placeholder="例如: my-skill" :input-props="{ autocapitalize: 'off' }" />
        </n-form-item>
        <n-form-item label="描述">
          <n-input v-model:value="createSkillForm.description" placeholder="技能描述" />
        </n-form-item>
        <n-form-item label="SKILL.md">
          <n-input
            v-model:value="createSkillForm.skill_md_content"
            type="textarea"
            placeholder="输入 SKILL.md 内容"
            :rows="6"
          />
        </n-form-item>
      </n-form>
    </n-modal>

    <!-- Install from URL Modal -->
    <n-modal
      v-model:show="showUrlInstallModal"
      preset="dialog"
      title="从 URL 安装技能"
      positive-text="安装"
      negative-text="取消"
      :loading="urlInstallLoading"
      @positive-click="handleUrlInstall"
      style="width: 480px"
    >
      <n-form :model="urlInstallForm" label-placement="left" label-width="80">
        <n-form-item label="目标源" required>
          <n-select
            v-model:value="urlInstallForm.sourceId"
            :options="writableSourceSelectOptions"
            placeholder="选择目标源"
          />
        </n-form-item>
        <n-form-item label="URL" required>
          <n-input v-model:value="urlInstallForm.url" placeholder="技能 URL 地址" :input-props="{ autocapitalize: 'off' }" />
        </n-form-item>
      </n-form>
    </n-modal>

    <!-- Install to Target Sources Modal -->
    <n-modal
      v-model:show="showInstallTargetModal"
      preset="dialog"
      :title="`安装技能 - ${installTargetSkill?.name || ''}`"
      positive-text="安装"
      negative-text="取消"
      :loading="installTargetLoading"
      @positive-click="handleInstallToTargets"
      style="width: 480px"
    >
      <n-space vertical>
        <n-text>选择要安装到的目标源：</n-text>
        <n-checkbox-group v-model:value="installTargetSourceIds">
          <n-space vertical>
            <n-checkbox
              v-for="source in installableSources"
              :key="source.id"
              :value="source.id"
              :label="source.name"
            />
          </n-space>
        </n-checkbox-group>
      </n-space>
    </n-modal>

    <!-- Edit SKILL.md Modal -->
    <n-modal
      v-model:show="showEditMdModal"
      preset="dialog"
      :title="`编辑 SKILL.md - ${editMdSkill?.name || ''}`"
      positive-text="保存"
      negative-text="取消"
      :loading="editMdLoading"
      @positive-click="handleSaveMd"
      style="width: 680px"
    >
      <n-spin :show="editMdContentLoaded">
        <n-input
          v-model:value="editMdContent"
          type="textarea"
          placeholder="SKILL.md 内容"
          :rows="16"
        />
      </n-spin>
    </n-modal>
  </n-space>
</template>

<script setup lang="ts">
import { ref, computed, h, onMounted } from 'vue'
import {
  NTag,
  NPopconfirm,
  NButton,
  NSpace,
  NTooltip,
  useMessage,
} from 'naive-ui'
import { api } from '../api'
import type {
  SkillSourceWithCount,
  Skill,
  SkillDetail,
  CreateSkillSourceBody,
  CreateSkillBody,
  InstallFromUrlBody,
} from '../types'

const message = useMessage()
const loading = ref(false)
const scanLoading = ref(false)
const discoverLoading = ref(false)
const sources = ref<SkillSourceWithCount[]>([])
const skills = ref<Skill[]>([])
const activeTab = ref<string>('')

// Add Source Modal
const showAddSourceModal = ref(false)
const addSourceLoading = ref(false)
const addSourceForm = ref({ name: '', path: '' })

// Create Skill Modal
const showCreateSkillModal = ref(false)
const createSkillLoading = ref(false)
const createSkillForm = ref({
  sourceId: '',
  name: '',
  description: '',
  skill_md_content: '',
})

// URL Install Modal
const showUrlInstallModal = ref(false)
const urlInstallLoading = ref(false)
const urlInstallForm = ref({ sourceId: '', url: '' })

// Install Target Modal
const showInstallTargetModal = ref(false)
const installTargetLoading = ref(false)
const installTargetSkill = ref<Skill | null>(null)
const installTargetSourceIds = ref<string[]>([])

// Edit MD Modal
const showEditMdModal = ref(false)
const editMdLoading = ref(false)
const editMdContentLoaded = ref(false)
const editMdSkill = ref<Skill | null>(null)
const editMdContent = ref('')

// Dropdown options for install button
const installOptions = [
  { label: '从全局库安装', key: 'global' },
  { label: '从 URL 安装', key: 'url' },
]

// Computed source select options
const sourceSelectOptions = computed(() =>
  sources.value.map((s) => ({ label: s.name, value: s.id }))
)

const writableSourceSelectOptions = computed(() =>
  sources.value.filter((s) => !s.is_global).map((s) => ({ label: s.name, value: s.id }))
)

// Sources available for install targets (exclude global library)
const installableSources = computed(() =>
  sources.value.filter((s) => !s.is_global)
)

// Get global source
const globalSource = computed(() =>
  sources.value.find((s) => s.is_global)
)

// Get skills for a given source
function getSkillsForSource(sourceId: string): Skill[] {
  return skills.value.filter((s) => s.source_id === sourceId)
}

// Helper to normalize boolean from SQLite (0/1 -> boolean)
function toBool(val: unknown): boolean {
  if (typeof val === 'boolean') return val
  if (typeof val === 'number') return val !== 0
  if (typeof val === 'string') return val === '1' || val.toLowerCase() === 'true'
  return false
}

// Table columns
const columns = [
  {
    title: '名称',
    key: 'name',
    width: 160,
    render: (row: Skill) =>
      h(NTooltip, {}, {
        trigger: () => h('span', {}, row.name),
        default: () => row.description || row.name,
      }),
  },
  {
    title: '描述',
    key: 'description',
    ellipsis: { tooltip: true },
    render: (row: Skill) => {
      const desc = row.description || ''
      if (desc.length <= 60) return desc || '-'
      return desc.substring(0, 60) + '...'
    },
  },
  {
    title: '类型',
    key: 'type',
    width: 100,
    render: (row: Skill) => {
      const source = sources.value.find((s) => s.id === row.source_id)
      if (source && toBool(source.is_global)) {
        return h(NTag, { size: 'small', type: 'info' as never }, () => '全局')
      }
      if (toBool(row.is_symlink)) {
        return h(NTag, { size: 'small', type: 'warning' as never }, () => '链接')
      }
      return h(NTag, { size: 'small', type: 'success' as never }, () => '本地')
    },
  },
  {
    title: '链接目标',
    key: 'link_target',
    width: 200,
    ellipsis: { tooltip: true },
    render: (row: Skill) => {
      if (!toBool(row.is_symlink) || !row.link_target) return '-'
      return row.link_target
    },
  },
  {
    title: '操作',
    key: 'actions',
    width: 280,
    fixed: 'right' as const,
    render: (row: Skill) => {
      const source = sources.value.find((s) => s.id === row.source_id)
      const isGlobal = source ? toBool(source.is_global) : false
      const isSymlink = toBool(row.is_symlink)

      const buttons: ReturnType<typeof h>[] = []

      // Install to... (only for global skills)
      if (isGlobal) {
        buttons.push(
          h(
            NButton,
            { size: 'small', quaternary: true, type: 'primary', onClick: () => openInstallTargetModal(row) },
            () => '安装到...'
          )
        )
      }

      // Edit SKILL.md
      if (toBool(row.has_skill_md)) {
        buttons.push(
          h(
            NButton,
            { size: 'small', quaternary: true, type: 'info', onClick: () => openEditMdModal(row) },
            () => '编辑 MD'
          )
        )
      }

      // Uninstall (symlink only) or Delete
      if (isSymlink) {
        buttons.push(
          h(
            NPopconfirm,
            { onPositiveClick: () => handleUninstall(row.id) },
            {
              trigger: () =>
                h(NButton, { size: 'small', quaternary: true, type: 'warning' }, () => '卸载'),
              default: () => '确认卸载此技能？仅删除符号链接。',
            }
          )
        )
      } else if (!isGlobal) {
        buttons.push(
          h(
            NPopconfirm,
            { onPositiveClick: () => handleDeleteSkill(row.id) },
            {
              trigger: () =>
                h(NButton, { size: 'small', quaternary: true, type: 'error' }, () => '删除'),
              default: () => '确认删除此技能？',
            }
          )
        )
      }

      return h(NSpace, { size: 4 }, () => buttons)
    },
  },
]

// Tab change handler
function handleTabChange(value: string) {
  activeTab.value = value
}

// ---- Data Fetching ----

async function handleScan() {
  scanLoading.value = true
  try {
    await api('/api/skills/scan', { method: 'POST' })
    await fetchData()
    message.success('扫描完成')
  } catch (err) {
    message.error(`扫描失败: ${err}`)
  } finally {
    scanLoading.value = false
  }
}

async function handleAutoDiscover() {
  discoverLoading.value = true
  try {
    await api('/api/skills/discover', { method: 'POST' })
    await fetchData()
    message.success('自动发现完成')
  } catch (err) {
    message.error(`自动发现失败: ${err}`)
  } finally {
    discoverLoading.value = false
  }
}

async function fetchData() {
  loading.value = true
  try {
    const [sourceList, skillList] = await Promise.all([
      api<SkillSourceWithCount[]>('/api/skills/sources'),
      api<Skill[]>('/api/skills'),
    ])
    sources.value = sourceList
    skills.value = skillList

    // Set active tab to first source if not set or current tab no longer valid
    if (sources.value.length > 0) {
      const validIds = sources.value.map((s) => s.id)
      if (!activeTab.value || !validIds.includes(activeTab.value)) {
        activeTab.value = sources.value[0].id
      }
    } else {
      activeTab.value = ''
    }
  } catch (err) {
    message.error(`加载数据失败: ${err}`)
  } finally {
    loading.value = false
  }
}

// ---- Add Source ----

function openAddSourceModal() {
  addSourceForm.value = { name: '', path: '' }
  showAddSourceModal.value = true
}

async function handleAddSource() {
  if (!addSourceForm.value.name.trim()) {
    message.warning('请填写名称')
    return false
  }
  if (!addSourceForm.value.path.trim()) {
    message.warning('请填写路径')
    return false
  }

  addSourceLoading.value = true
  try {
    const body: CreateSkillSourceBody = {
      name: addSourceForm.value.name.trim(),
      path: addSourceForm.value.path.trim(),
    }
    await api('/api/skills/sources', {
      method: 'POST',
      body: JSON.stringify(body),
    })
    message.success('技能源添加成功')
    showAddSourceModal.value = false
    await fetchData()
  } catch (err) {
    message.error(`添加失败: ${err}`)
  } finally {
    addSourceLoading.value = false
  }
  return false
}

// ---- Create Skill ----

function openCreateSkillModal() {
  const writableSources = sources.value.filter((s) => !toBool(s.is_global))
  createSkillForm.value = {
    sourceId: writableSources.length > 0 ? writableSources[0].id : '',
    name: '',
    description: '',
    skill_md_content: '',
  }
  showCreateSkillModal.value = true
}

async function handleCreateSkill() {
  if (!createSkillForm.value.sourceId) {
    message.warning('请选择目标源')
    return false
  }
  if (!createSkillForm.value.name.trim()) {
    message.warning('请填写技能名称')
    return false
  }

  createSkillLoading.value = true
  try {
    const body: CreateSkillBody & { source_id: string } = {
      source_id: createSkillForm.value.sourceId,
      name: createSkillForm.value.name.trim(),
      description: createSkillForm.value.description.trim() || undefined,
      skill_md_content: createSkillForm.value.skill_md_content.trim() || undefined,
    }
    await api('/api/skills', {
      method: 'POST',
      body: JSON.stringify(body),
    })
    message.success('技能创建成功')
    showCreateSkillModal.value = false
    await fetchData()
  } catch (err) {
    message.error(`创建失败: ${err}`)
  } finally {
    createSkillLoading.value = false
  }
  return false
}

// ---- Install from URL ----

function openUrlInstallModal() {
  const writable = sources.value.filter((s) => !toBool(s.is_global))
  urlInstallForm.value = {
    sourceId: writable.length > 0 ? writable[0].id : '',
    url: '',
  }
  showUrlInstallModal.value = true
}

async function handleUrlInstall() {
  if (!urlInstallForm.value.sourceId) {
    message.warning('请选择目标源')
    return false
  }
  if (!urlInstallForm.value.url.trim()) {
    message.warning('请填写 URL')
    return false
  }

  urlInstallLoading.value = true
  try {
    const body: InstallFromUrlBody & { source_id: string } = {
      source_id: urlInstallForm.value.sourceId,
      url: urlInstallForm.value.url.trim(),
    }
    await api('/api/skills/install-from-url', {
      method: 'POST',
      body: JSON.stringify(body),
    })
    message.success('技能安装成功')
    showUrlInstallModal.value = false
    await fetchData()
  } catch (err) {
    message.error(`安装失败: ${err}`)
  } finally {
    urlInstallLoading.value = false
  }
  return false
}

// ---- Install to Target Sources ----

function openInstallTargetModal(skill: Skill) {
  installTargetSkill.value = skill
  // Pre-select non-global sources
  installTargetSourceIds.value = installableSources.value.map((s) => s.id)
  showInstallTargetModal.value = true
}

async function handleInstallToTargets() {
  if (!installTargetSkill.value) return false
  if (installTargetSourceIds.value.length === 0) {
    message.warning('请至少选择一个目标源')
    return false
  }

  installTargetLoading.value = true
  try {
    await api(`/api/skills/${installTargetSkill.value.id}/install`, {
      method: 'POST',
      body: JSON.stringify({
        skill_id: installTargetSkill.value.id,
        target_source_ids: installTargetSourceIds.value,
      }),
    })
    message.success('技能安装成功')
    showInstallTargetModal.value = false
    await fetchData()
  } catch (err) {
    message.error(`安装失败: ${err}`)
  } finally {
    installTargetLoading.value = false
  }
  return false
}

// ---- Uninstall Skill (remove symlink) ----

async function handleUninstall(skillId: string) {
  try {
    await api(`/api/skills/${skillId}/uninstall`, {
      method: 'POST',
      body: JSON.stringify({ skill_id: skillId }),
    })
    message.success('技能已卸载')
    await fetchData()
  } catch (err) {
    message.error(`卸载失败: ${err}`)
  }
}

// ---- Delete Skill ----

async function handleDeleteSkill(skillId: string) {
  try {
    await api(`/api/skills/${skillId}`, { method: 'DELETE' })
    message.success('技能已删除')
    await fetchData()
  } catch (err) {
    message.error(`删除失败: ${err}`)
  }
}

// ---- Edit SKILL.md ----

async function openEditMdModal(skill: Skill) {
  editMdSkill.value = skill
  editMdContent.value = ''
  editMdContentLoaded.value = true
  showEditMdModal.value = true

  try {
    const detail = await api<SkillDetail>(`/api/skills/${skill.id}`)
    editMdContent.value = detail.skill_md_content || ''
  } catch (err) {
    message.error(`加载 SKILL.md 失败: ${err}`)
  } finally {
    editMdContentLoaded.value = false
  }
}

async function handleSaveMd() {
  if (!editMdSkill.value) return false

  editMdLoading.value = true
  try {
    await api(`/api/skills/${editMdSkill.value.id}/skill-md`, {
      method: 'PUT',
      body: JSON.stringify({ content: editMdContent.value }),
    })
    message.success('SKILL.md 已保存')
    showEditMdModal.value = false
    await fetchData()
  } catch (err) {
    message.error(`保存失败: ${err}`)
  } finally {
    editMdLoading.value = false
  }
  return false
}

// ---- Install dropdown handler ----

function handleInstallSelect(key: string) {
  if (key === 'global') {
    // Open install target modal with global skills list
    if (!globalSource.value) {
      message.warning('未找到全局技能库')
      return
    }
    // If there are global skills, let user pick one
    const globalSkills = getSkillsForSource(globalSource.value.id)
    if (globalSkills.length === 0) {
      message.warning('全局技能库中没有技能')
      return
    }
    // Open install target modal for first global skill as example
    // User can select which skills to install from the global source tab
    openInstallTargetModal(globalSkills[0])
  } else if (key === 'url') {
    openUrlInstallModal()
  }
}

// ---- Lifecycle ----

onMounted(async () => {
  // Auto scan on page load, then fetch data
  scanLoading.value = true
  try {
    await api('/api/skills/scan', { method: 'POST' })
  } catch {
    // Ignore scan errors on initial load
  } finally {
    scanLoading.value = false
  }
  await fetchData()
})
</script>
