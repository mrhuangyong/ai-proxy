<template>
  <n-space vertical size="large">
    <n-card>
      <template #header>
        <n-text strong>请求日志</n-text>
      </template>
      <template #header-extra>
        <n-space align="center">
          <n-input
            v-model:value="searchQuery"
            placeholder="按模型名搜索"
            clearable
            style="width: 220px"
            @keyup.enter="handleQuery"
          >
            <template #prefix>
              <span style="opacity: 0.5">🔍</span>
            </template>
          </n-input>
          <n-date-picker
            v-model:value="dateRange"
            type="daterange"
            clearable
            style="width: 280px"
          />
          <n-button type="primary" @click="handleQuery">查询</n-button>
          <n-button type="error" @click="handleClearLogs">清除日志</n-button>
        </n-space>
      </template>
      <n-data-table
        :columns="columns"
        :data="logs"
        :loading="loading"
        :bordered="false"
        remote
        :scroll-x="1600"
        :pagination="pagination"
        :row-key="(row: RequestLog) => row.id"
        @update:page="handlePageChange"
        @update:page-size="handlePageSizeChange"
      />
    </n-card>
    <n-modal v-model:show="showUsageModal" preset="card" :title="usageModalTitle" style="width: 760px">
      <n-tabs type="line" animated>
        <n-tab-pane name="final" tab="最终 Usage">
          <n-button size="small" style="margin-bottom: 8px" @click="copyText(finalUsagePretty)">复制</n-button>
          <pre style="white-space: pre-wrap; word-break: break-all; margin: 0; font-family: 'SF Mono', Menlo, monospace; font-size: 12px; max-height: 50vh; overflow-y: auto">{{ finalUsagePretty || '(无数据)' }}</pre>
        </n-tab-pane>
        <n-tab-pane name="events" tab="上游事件 Usage">
          <n-button size="small" style="margin-bottom: 8px" @click="copyText(upstreamEventsPretty)">复制</n-button>
          <pre style="white-space: pre-wrap; word-break: break-all; margin: 0; font-family: 'SF Mono', Menlo, monospace; font-size: 12px; max-height: 50vh; overflow-y: auto">{{ upstreamEventsPretty || '(无数据)' }}</pre>
        </n-tab-pane>
      </n-tabs>
    </n-modal>
    <n-modal v-model:show="showErrorModal" preset="card" title="错误详情" style="width: 600px">
      <div style="max-height: 50vh; overflow-y: auto">
        <pre style="white-space: pre-wrap; word-break: break-all; margin: 0; font-family: inherit; font-size: 14px;">{{ errorDetail }}</pre>
      </div>
    </n-modal>
  </n-space>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, h } from 'vue'
import { api } from '../api'
import { formatDateTime } from '../utils/format'
import { NTag, NSpace, NTooltip, NModal, NTabs, NTabPane, useDialog, useMessage } from 'naive-ui'
import type { RequestLog } from '../types'

const dialog = useDialog()
const message = useMessage()

const showErrorModal = ref(false)
const errorDetail = ref('')

const showUsageModal = ref(false)
const usageModalTitle = ref('上游 Usage 详情')
const finalUsageRaw = ref<string | null>(null)
const upstreamEventsRaw = ref<string | null>(null)

const finalUsagePretty = computed(() => {
  if (!finalUsageRaw.value) return ''
  try {
    return JSON.stringify(JSON.parse(finalUsageRaw.value), null, 2)
  } catch {
    return finalUsageRaw.value
  }
})

const upstreamEventsPretty = computed(() => {
  if (!upstreamEventsRaw.value) return ''
  try {
    return JSON.stringify(JSON.parse(upstreamEventsRaw.value), null, 2)
  } catch {
    return upstreamEventsRaw.value
  }
})

function copyText(text: string) {
  if (!text) return
  navigator.clipboard.writeText(text).then(
    () => message.success('已复制'),
    () => message.error('复制失败'),
  )
}

function openUsageDetail(row: RequestLog) {
  finalUsageRaw.value = row.final_usage_json ?? null
  upstreamEventsRaw.value = row.upstream_usage_events_json ?? null
  usageModalTitle.value = `上游 Usage - ${row.model} #${row.id}`
  showUsageModal.value = true
}

const loading = ref(false)
const logs = ref<RequestLog[]>([])
const searchQuery = ref('')
const dateRange = ref<[number, number] | null>(null)
const currentPage = ref(1)
const pageSize = ref(10)
const total = ref(0)

const pagination = computed(() => ({
  page: currentPage.value,
  pageSize: pageSize.value,
  itemCount: total.value,
  showSizePicker: true,
  pageSizes: [10, 20, 50, 100],
  prefix: ({ itemCount }: { itemCount: number }) => `共 ${itemCount} 条`,
}))

function statusCodeColor(code: number): 'success' | 'warning' | 'error' {
  if (code < 300) return 'success'
  if (code < 400) return 'warning'
  return 'error'
}

function formatUtcTime(utcStr: string): string {
  return formatDateTime(utcStr, { utc: true })
}

function formatNumber(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`
  return String(n)
}

const columns = [
  {
    title: '时间',
    key: 'created_at',
    width: 180,
    render: (row: RequestLog) => formatUtcTime(row.created_at),
  },
  {
    title: '模型',
    key: 'model',
    width: 200,
    render: (row: RequestLog) => {
      // Show "请求模型 -> 目标模型" when they differ, otherwise just the model name.
      const tm = row.target_model || ''
      if (tm && tm !== row.model) {
        return h('span', { style: 'white-space: pre-wrap; word-break: break-all' }, [
          row.model,
          ' → ',
          tm,
        ])
      }
      return row.model
    },
  },
  { title: '供应商', key: 'provider_name', width: 100 },
  {
    title: '请求格式',
    key: 'format',
    width: 170,
    render: (row: RequestLog) =>
      h(NSpace, { size: 4, align: 'center' }, () => [
        h(NTag, { size: 'small', type: 'info' }, () => row.client_format),
        h('span', { style: 'color: var(--text-3); font-size: 12px' }, () => '→'),
        h(NTag, { size: 'small', type: 'warning' }, () => row.provider_format),
      ]),
  },
  {
    title: '状态码',
    key: 'status_code',
    width: 80,
    render: (row: RequestLog) => {
      const code = row.status_code ?? 0
      const tag = h(NTag, { size: 'small', type: statusCodeColor(code), style: code !== 200 && row.error_message ? 'cursor: pointer' : undefined }, () => String(code))
      if (code !== 200 && row.error_message) {
        return h(NTooltip, { trigger: 'hover' }, { trigger: () => h('span', { onClick: () => openErrorDetail(row.error_message!) }, tag), default: () => '点击查看详情' })
      }
      return tag
    },
  },
  {
    title: '输入/缓存',
    key: 'prompt_tokens',
    width: 130,
    render: (row: RequestLog) =>
      h(NSpace, { size: 4 }, () => [
        h(NTag, { size: 'small' }, () => formatNumber(row.prompt_tokens)),
        h(NTag, { size: 'small', type: 'info' }, () => formatNumber(row.cached_tokens)),
      ]),
  },
  {
    title: '命中',
    key: 'cache_hit',
    width: 90,
    render: (row: RequestLog) => {
      let label: string
      let type: 'success' | 'warning' | 'default' = 'default'
      if (row.prompt_tokens === 0) {
        label = '查看'
      } else {
        const rate = row.cached_tokens / row.prompt_tokens * 100
        label = `${rate.toFixed(2)}%`
        type = rate >= 50 ? 'success' : rate > 0 ? 'warning' : 'default'
      }
      return h(
        NTooltip,
        { trigger: 'hover' },
        {
          trigger: () =>
            h(
              NTag,
              {
                size: 'small',
                type,
                style: 'cursor: pointer',
                onClick: () => openUsageDetail(row),
              },
              () => label,
            ),
          default: () => '点击查看 Usage 详情',
        },
      )
    },
  },
  { title: '输出', key: 'completion_tokens', width: 100, render: (row: RequestLog) => formatNumber(row.completion_tokens) },
  {
    title: '用时/首字',
    key: 'ttft_ms',
    width: 130,
    render: (row: RequestLog) =>
      h(NSpace, { size: 4 }, () => [
        h(NTag, { size: 'small' }, () => row.duration_ms != null ? `${(row.duration_ms / 1000).toFixed(1)}s` : '-'),
        h(NTag, { size: 'small', type: 'info' }, () => row.ttft_ms != null ? `${(row.ttft_ms / 1000).toFixed(1)}s` : '-'),
      ]),
  },
  {
    title: '流式',
    key: 'stream',
    width: 60,
    render: (row: RequestLog) =>
      h(NTag, { size: 'small', type: row.stream ? 'success' : 'default' }, () => (row.stream ? '是' : '否')),
  },
  {
    title: '客户端 UA',
    key: 'client_user_agent',
    width: 180,
    render: (row: RequestLog) => {
      const ua = row.client_user_agent || ''
      if (!ua) return '-'
      const span = h(
        'span',
        {
          style:
            'display: inline-block; max-width: 150px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; vertical-align: bottom; cursor: pointer',
          onClick: () => copyText(ua),
        },
        ua,
      )
      return h(
        NTooltip,
        { trigger: 'hover' },
        {
          trigger: () => span,
          default: () => '点击复制：' + ua,
        },
      )
    },
  },
]

function handlePageChange(page: number) {
  currentPage.value = page
  fetchLogs()
}

function handlePageSizeChange(size: number) {
  pageSize.value = size
  currentPage.value = 1
  fetchLogs()
}

function handleQuery() {
  currentPage.value = 1
  fetchLogs()
}

async function fetchLogs() {
  loading.value = true
  try {
    let url = `/api/logs?page=${currentPage.value}&limit=${pageSize.value}`
    if (searchQuery.value.trim()) {
      url += `&model=${encodeURIComponent(searchQuery.value.trim())}`
    }
    if (dateRange.value) {
      const start = new Date(dateRange.value[0])
      const end = new Date(dateRange.value[1])
      url += `&start_date=${formatDate(start)}`
      url += `&end_date=${formatDate(end)}`
    }
    const result = await api<{ logs: RequestLog[]; total: number }>(url)
    logs.value = result.logs
    total.value = result.total
  } catch (error) {
    console.error('Failed to load logs:', error)
  } finally {
    loading.value = false
  }
}

function formatDate(d: Date): string {
  const y = d.getFullYear()
  const m = String(d.getMonth() + 1).padStart(2, '0')
  const day = String(d.getDate()).padStart(2, '0')
  return `${y}-${m}-${day}`
}

function openErrorDetail(msg: string) {
  errorDetail.value = msg
  showErrorModal.value = true
}

async function handleClearLogs() {
  dialog.warning({
    title: '清除日志',
    content: '确定要清除所有日志吗？此操作不可恢复。',
    positiveText: '确定清除',
    negativeText: '取消',
    onPositiveClick: async () => {
      try {
        await api<{ deleted: boolean }>('/api/logs', { method: 'DELETE' })
        logs.value = []
        total.value = 0
      } catch (error) {
        console.error('Failed to clear logs:', error)
      }
    },
  })
}

onMounted(() => {
  fetchLogs()
})
</script>
