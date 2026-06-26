<template>
  <n-config-provider :locale="zhCN" :date-locale="dateZhCN" :theme="naiveTheme" :theme-overrides="themeOverrides">
    <n-dialog-provider>
    <n-message-provider>
    <n-layout v-if="isLoginRoute" style="height: 100vh">
      <router-view />
    </n-layout>
    <n-layout v-else style="height: 100vh" has-sider>
      <n-layout-sider
        collapse-mode="width"
        :collapsed-width="64"
        :width="220"
        :collapsed="collapsed"
        show-trigger
        :native-scrollbar="false"
        :style="{ background: 'var(--sidebar-bg)' }"
        @collapse="collapsed = true"
        @expand="collapsed = false"
      >
        <div class="sidebar-logo" data-tauri-drag-region>
          <div class="sidebar-logo-icon">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
              <path d="M12 2L2 7l10 5 10-5-10-5z"/>
              <path d="M2 17l10 5 10-5"/>
              <path d="M2 12l10 5 10-5"/>
            </svg>
          </div>
          <span v-show="!collapsed" class="sidebar-logo-text" data-tauri-drag-region>AI Proxy</span>
        </div>

        <div v-if="!collapsed" class="sidebar-section-label">概览</div>
        <n-menu
          :collapsed="collapsed"
          :collapsed-width="64"
          :collapsed-icon-size="20"
          :options="overviewMenu"
          :value="currentPath"
          :indent="20"
          @update:value="handleMenuSelect"
        />

        <div v-if="!collapsed" class="sidebar-section-label">管理</div>
        <n-menu
          :collapsed="collapsed"
          :collapsed-width="64"
          :collapsed-icon-size="20"
          :options="manageMenu"
          :value="currentPath"
          :indent="20"
          @update:value="handleMenuSelect"
        />

        <div v-if="!collapsed" class="sidebar-section-label">日志</div>
        <n-menu
          :collapsed="collapsed"
          :collapsed-width="64"
          :collapsed-icon-size="20"
          :options="logsMenu"
          :value="currentPath"
          :indent="20"
          @update:value="handleMenuSelect"
        />

        <div v-if="!collapsed" class="sidebar-section-label">系统</div>
        <n-menu
          :collapsed="collapsed"
          :collapsed-width="64"
          :collapsed-icon-size="20"
          :options="systemMenu"
          :value="currentPath"
          :indent="20"
          @update:value="handleMenuSelect"
        />

      </n-layout-sider>
      <n-layout class="main-layout">
        <div class="header-bar" data-tauri-drag-region>
          <span data-tauri-drag-region>&nbsp;</span>
          <n-space align="center" size="small">
            <n-button quaternary size="tiny" @click="toggleTheme" style="color: var(--text-2)">
              <template #icon>
                <n-icon size="16"><component :is="isDark ? SunnyOutline : MoonOutline" /></n-icon>
              </template>
            </n-button>
            <n-space align="center" size="small" :style="{ color: serverRunning ? 'var(--success)' : 'var(--error)', fontSize: '12px', fontFamily: 'var(--font-mono)' }">
              <span class="status-dot" :class="serverRunning ? 'running' : 'stopped'" />
              <span>{{ serverRunning ? `127.0.0.1:${proxyPort}` : '已停止' }}</span>
            </n-space>
            <template v-if="!isTauri">
              <n-button quaternary size="tiny" @click="handleLogout" style="color: var(--text-2)">
                <template #icon>
                  <n-icon size="16"><LogOutOutline /></n-icon>
                </template>
                退出
              </n-button>
            </template>
          </n-space>
        </div>
        <n-layout-content content-style="padding: 20px 24px;">
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
import { apiState, initApi, setUnauthorizedHandler } from './api'
import { useAuthStore } from './stores/auth'
import { useTheme } from './theme/use-theme'
import UpdateNotification from './components/UpdateNotification.vue'
import {
  HomeOutline,
  CloudOutline,
  GitNetworkOutline,
  DocumentTextOutline,
  TerminalOutline,
  AppsOutline,
  SettingsOutline,
  BookOutline,
  SunnyOutline,
  MoonOutline,
  LogOutOutline,
  SwapHorizontalOutline,
} from '@vicons/ionicons5'

const { isDark, naiveTheme, themeOverrides, toggleTheme } = useTheme()

const router = useRouter()
const route = useRoute()
const collapsed = ref(false)
const serverRunning = computed(() => apiState.initialized)
const proxyPort = computed(() => apiState.proxyPort)
const updateNotification = ref<InstanceType<typeof UpdateNotification> | null>(null)
const authStore = useAuthStore()

const isLoginRoute = computed(() => route.name === 'Login')

async function handleLogout() {
  await authStore.logout()
  router.replace({ name: 'Login' })
}

setUnauthorizedHandler(() => {
  router.replace({ name: 'Login' })
})

onMounted(async () => {
  try {
    await initApi()
  } catch {
    apiState.initialized = false
  }

  if (!isTauri) {
    await authStore.fetchMe()
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

const overviewMenu = [
  { label: '仪表盘', key: '/', icon: renderIcon(HomeOutline) },
]

const manageMenu = computed(() => {
  const items = [
    { label: '供应商', key: '/providers', icon: renderIcon(CloudOutline) },
    { label: '虚拟模型', key: '/virtual-models', icon: renderIcon(SwapHorizontalOutline) },
    { label: '拦截规则', key: '/rules', icon: renderIcon(SettingsOutline) },
  ]
  if (isTauri) {
    items.splice(2, 0,
      { label: '应用管理', key: '/apps', icon: renderIcon(AppsOutline) },
      { label: 'MCP 管理', key: '/mcp', icon: renderIcon(GitNetworkOutline) },
      { label: '技能管理', key: '/skills', icon: renderIcon(BookOutline) },
    )
  }
  return items
})

const logsMenu = [
  { label: '请求日志', key: '/logs', icon: renderIcon(DocumentTextOutline) },
  { label: '运行日志', key: '/runtime-logs', icon: renderIcon(TerminalOutline) },
]

const systemMenu = [
  { label: '设置', key: '/settings', icon: renderIcon(SettingsOutline) },
]

const currentPath = computed(() => route.path)

function handleMenuSelect(key: string) {
  router.push(key)
}
</script>
