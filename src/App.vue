<template>
  <n-config-provider :locale="zhCN" :date-locale="dateZhCN">
    <n-dialog-provider>
    <n-message-provider>
    <n-layout style="height: 100vh" has-sider>
      <n-layout-sider
        bordered
        collapse-mode="width"
        :collapsed-width="64"
        :width="200"
        :collapsed="collapsed"
        show-trigger
        @collapse="collapsed = true"
        @expand="collapsed = false"
      >
        <n-menu
          :collapsed="collapsed"
          :collapsed-width="64"
          :collapsed-icon-size="22"
          :options="menuOptions"
          :value="currentPath"
          @update:value="handleMenuSelect"
        />
      </n-layout-sider>
      <n-layout>
        <n-layout-header bordered style="padding: 12px 24px; display: flex; align-items: center; justify-content: flex-end;">
          <n-tag :type="serverRunning ? 'success' : 'error'" size="small">
            {{ serverRunning ? `Proxy :${proxyPort}` : 'Proxy stopped' }}
          </n-tag>
        </n-layout-header>
        <n-layout-content content-style="padding: 24px;">
          <router-view />
        </n-layout-content>
      </n-layout>
    </n-layout>
    <UpdateNotification ref="updateNotification" />
    </n-message-provider>
    </n-dialog-provider>
  </n-config-provider>
</template>

<script setup lang="ts">
import { ref, computed, h, onMounted } from 'vue'
import { useRouter, useRoute } from 'vue-router'
import { NIcon, zhCN, dateZhCN } from 'naive-ui'
import { listen } from '@tauri-apps/api/event'
import { isTauri } from './utils/env'
import { apiState, initApi } from './api'
import UpdateNotification from './components/UpdateNotification.vue'
import {
  HomeOutline,
  ServerOutline,
  DocumentTextOutline,
  TerminalOutline,
  BarChartOutline,
  AppsOutline,
  SettingsOutline,
  BookOutline,
} from '@vicons/ionicons5'

const router = useRouter()
const route = useRoute()
const collapsed = ref(false)
const serverRunning = computed(() => apiState.initialized)
const proxyPort = computed(() => apiState.proxyPort)
const updateNotification = ref<InstanceType<typeof UpdateNotification> | null>(null)

onMounted(async () => {
  try {
    await initApi()
  } catch {
    apiState.initialized = false
  }

  if (isTauri) {
    listen<{ version: string; release_notes: string; download_url: string; published_at: string }>('update-available', (event) => {
      updateNotification.value?.show(event.payload)
    })
  }
})

function renderIcon(icon: typeof HomeOutline) {
  return () => h(NIcon, null, () => h(icon))
}

const menuOptions = [
  { label: '仪表盘', key: '/', icon: renderIcon(HomeOutline) },
  { label: '供应商', key: '/providers', icon: renderIcon(ServerOutline) },
  { label: '应用管理', key: '/apps', icon: renderIcon(AppsOutline) },
  { label: 'MCP 管理', key: '/mcp', icon: renderIcon(ServerOutline) },
  { label: '技能管理', key: '/skills', icon: renderIcon(BookOutline) },
  { label: '请求日志', key: '/logs', icon: renderIcon(DocumentTextOutline) },
  { label: '用量统计', key: '/statistics', icon: renderIcon(BarChartOutline) },
  { label: '运行日志', key: '/runtime-logs', icon: renderIcon(TerminalOutline) },
  { label: '拦截规则', key: '/rules', icon: renderIcon(SettingsOutline) },
  { label: '设置', key: '/settings', icon: renderIcon(SettingsOutline) },
]

const currentPath = computed(() => route.path)

function handleMenuSelect(key: string) {
  router.push(key)
}
</script>
