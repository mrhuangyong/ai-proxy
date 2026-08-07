<template>
  <n-modal
    v-model:show="visible"
    preset="card"
    title="发现新版本"
    style="max-width: 500px"
    :mask-closable="false"
  >
    <n-space vertical size="medium">
      <n-tag type="success" size="medium">
        v{{ updateInfo.version }}
      </n-tag>
      <n-text depth="3" style="font-size: 13px">
        发布于 {{ formatDate(updateInfo.published_at) }}
      </n-text>
      <n-divider style="margin: 8px 0" />
      <n-scrollbar style="max-height: 250px">
        <pre style="white-space: pre-wrap; word-break: break-word; font-size: 13px; margin: 0">{{ updateInfo.release_notes || '暂无更新说明' }}</pre>
      </n-scrollbar>

      <!-- 下载进度 -->
      <template v-if="downloadState.downloading || downloadState.done">
        <n-progress
          :percentage="downloadState.percent"
          :status="downloadState.done ? 'success' : 'default'"
          :show-indicator="true"
          :height="18"
          :border-radius="4"
        />
        <n-text v-if="downloadState.downloading && !downloadState.done" depth="3" style="font-size: 12px">
          {{ formatBytes(downloadState.downloaded) }} / {{ formatBytes(downloadState.total) }}
        </n-text>
        <n-text v-if="downloadState.done" type="success" style="font-size: 12px">
          下载完成，已保存到下载目录
        </n-text>
      </template>

      <!-- 错误提示 -->
      <n-text v-if="downloadState.error" type="error" style="font-size: 12px">
        {{ downloadState.error }}
      </n-text>
    </n-space>
    <template #footer>
      <n-space justify="end">
        <n-button v-if="!downloadState.downloading" @click="handleDismiss">
          {{ downloadState.done ? '关闭' : '稍后提醒' }}
        </n-button>
        <n-button
          v-if="!downloadState.downloading && !downloadState.done"
          type="primary"
          @click="handleDownload"
        >
          立即下载
        </n-button>
        <n-button
          v-if="downloadState.done"
          type="primary"
          @click="handleInstall"
        >
          安装更新
        </n-button>
      </n-space>
    </template>
  </n-modal>
</template>

<script setup lang="ts">
import { ref, reactive, onMounted, onUnmounted } from 'vue'
import { useMessage } from 'naive-ui'
import { listen } from '@tauri-apps/api/event'
import { invoke } from '@tauri-apps/api/core'
import { isTauri } from '../utils/env'
import { formatDateTime } from '../utils/format'

interface UpdateInfo {
  version: string
  release_notes: string
  download_url: string
  published_at: string
}

interface DownloadProgress {
  downloaded: number
  total: number
  percent: number
}

const message = useMessage()
const visible = ref(false)
const updateInfo = ref<UpdateInfo>({
  version: '',
  release_notes: '',
  download_url: '',
  published_at: '',
})

const downloadState = reactive({
  downloading: false,
  done: false,
  percent: 0,
  downloaded: 0,
  total: 0,
  error: '',
  filePath: '',
})

let unlistenProgress: (() => void) | null = null
let unlistenUpToDate: (() => void) | null = null

onMounted(async () => {
  if (!isTauri) return

  unlistenUpToDate = await listen('up-to-date', () => {
    message.success('已是最新版本')
  })

  unlistenProgress = await listen<DownloadProgress>('update-download-progress', (event) => {
    downloadState.percent = event.payload.percent
    downloadState.downloaded = event.payload.downloaded
    downloadState.total = event.payload.total
  })
})

onUnmounted(() => {
  unlistenProgress?.()
  unlistenUpToDate?.()
})

function show(info: UpdateInfo) {
  updateInfo.value = { ...info }
  // Reset download state
  downloadState.downloading = false
  downloadState.done = false
  downloadState.percent = 0
  downloadState.downloaded = 0
  downloadState.total = 0
  downloadState.error = ''
  downloadState.filePath = ''
  visible.value = true
}

function handleDismiss() {
  visible.value = false
}

async function handleDownload() {
  downloadState.downloading = true
  downloadState.error = ''

  try {
    const filePath = await invoke<string>('download_update', {
      url: updateInfo.value.download_url,
    })
    downloadState.filePath = filePath
    downloadState.done = true
    downloadState.downloading = false
  } catch (e) {
    downloadState.downloading = false
    downloadState.error = String(e)
  }
}

async function handleInstall() {
  try {
    await invoke('open_update_file', { path: downloadState.filePath })
  } catch (e) {
    downloadState.error = `打开文件失败: ${e}`
  }
}

function formatDate(dateStr: string): string {
  return formatDateTime(dateStr).slice(0, 10)
}

function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B'
  const k = 1024
  const sizes = ['B', 'KB', 'MB', 'GB']
  const i = Math.floor(Math.log(bytes) / Math.log(k))
  return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + ' ' + sizes[i]
}

defineExpose({ show })
</script>
