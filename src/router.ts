import { createRouter, createWebHashHistory } from 'vue-router'
import { isTauri } from './utils/env'

const routes = [
  { path: '/login', name: 'Login', component: () => import('./views/Login.vue'), meta: { public: true } },
  { path: '/', name: 'Dashboard', component: () => import('./views/Dashboard.vue') },
  { path: '/providers', name: 'Providers', component: () => import('./views/Providers.vue') },
  { path: '/virtual-models', name: 'VirtualModels', component: () => import('./views/VirtualModels.vue') },
  { path: '/logs', name: 'Logs', component: () => import('./views/Logs.vue') },
  { path: '/runtime-logs', name: 'RuntimeLogs', component: () => import('./views/RuntimeLogs.vue') },
  { path: '/statistics', name: 'Statistics', component: () => import('./views/Statistics.vue') },
  { path: '/apps', name: 'Apps', component: () => import('./views/Apps.vue') },
  { path: '/mcp', name: 'McpServers', component: () => import('./views/McpServers.vue') },
  { path: '/skills', name: 'Skills', component: () => import('./views/Skills.vue') },
  { path: '/rules', name: 'Rules', component: () => import('./views/Rules.vue') },
  { path: '/settings', name: 'Settings', component: () => import('./views/Settings.vue') },
]

const router = createRouter({ history: createWebHashHistory(), routes })

router.beforeEach((to) => {
  if (isTauri) return true
  const isLoginRoute = to.name === 'Login'
  const token = localStorage.getItem('ai_proxy_token')

  if (!token && !isLoginRoute) {
    return { name: 'Login', query: to.fullPath !== '/' ? { redirect: to.fullPath } : undefined }
  }
  if (token && isLoginRoute) {
    return { path: '/' }
  }
  return true
})

export default router
