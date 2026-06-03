import { createRouter, createWebHashHistory } from 'vue-router'

const routes = [
  { path: '/', name: 'Dashboard', component: () => import('./views/Dashboard.vue') },
  { path: '/providers', name: 'Providers', component: () => import('./views/Providers.vue') },
  { path: '/logs', name: 'Logs', component: () => import('./views/Logs.vue') },
  { path: '/runtime-logs', name: 'RuntimeLogs', component: () => import('./views/RuntimeLogs.vue') },
  { path: '/statistics', name: 'Statistics', component: () => import('./views/Statistics.vue') },
  { path: '/apps', name: 'Apps', component: () => import('./views/Apps.vue') },
  { path: '/mcp', name: 'McpServers', component: () => import('./views/McpServers.vue') },
  { path: '/rules', name: 'Rules', component: () => import('./views/Rules.vue') },
  { path: '/settings', name: 'Settings', component: () => import('./views/Settings.vue') },
]

export default createRouter({ history: createWebHashHistory(), routes })
