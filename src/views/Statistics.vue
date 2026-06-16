<template>
  <n-space vertical size="large">
    <n-card>
      <template #header>
        <n-space justify="space-between" align="center">
          <n-text strong>用量统计</n-text>
          <n-space align="center">
            <n-radio-group v-model:value="timeRange" size="small" @update:value="handleRangeChange">
              <n-radio-button value="today">今日</n-radio-button>
              <n-radio-button value="week">本周</n-radio-button>
              <n-radio-button value="month">本月</n-radio-button>
            </n-radio-group>
          </n-space>
        </n-space>
      </template>

      <n-grid :cols="2" :x-gap="24" :y-gap="24">
        <n-gi>
          <n-card title="Token 用量趋势" size="small">
            <div ref="lineChartRef" style="height: 360px" />
          </n-card>
        </n-gi>
        <n-gi>
          <n-card title="模型费用分布" size="small">
            <div ref="pieChartRef" style="height: 360px" />
          </n-card>
        </n-gi>
      </n-grid>
    </n-card>

    <n-card title="用量汇总">
      <n-data-table
        :columns="summaryColumns"
        :data="stats"
        :bordered="false"
        size="small"
      />
    </n-card>
  </n-space>
</template>

<script setup lang="ts">
import { ref, onMounted, onUnmounted, nextTick } from 'vue'
import { api } from '../api'
import * as echarts from 'echarts'
import { getEchartsTheme } from '../theme'
import { useTheme } from '../theme/use-theme'
import type { UsageTrendPoint } from '../types'

const { isDark } = useTheme()

interface UsageStat {
  model: string
  target_model: string
  provider_name: string
  prompt_tokens: number
  completion_tokens: number
  total_tokens: number
  cached_tokens: number
  cost_estimate: number
  request_count: number
}

interface UsageResponse {
  stats: UsageStat[]
  total_cost: number
  total_requests: number
}

const timeRange = ref<'today' | 'week' | 'month'>('month')
const stats = ref<UsageStat[]>([])

const lineChartRef = ref<HTMLElement | null>(null)
const pieChartRef = ref<HTMLElement | null>(null)
let lineChart: echarts.ECharts | null = null
let pieChart: echarts.ECharts | null = null

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`
  return n.toString()
}

const summaryColumns = [
  {
    title: '模型',
    key: 'model',
    width: 200,
    render: (row: UsageStat) => {
      const tm = row.target_model || ''
      if (tm && tm !== row.model) return `${row.model} → ${tm}`
      return row.model
    },
  },
  { title: '供应商', key: 'provider_name', width: 140 },
  { title: '请求次数', key: 'request_count', width: 100 },
  {
    title: 'Prompt Tokens',
    key: 'prompt_tokens',
    width: 120,
    render: (row: UsageStat) => formatTokens(row.prompt_tokens),
  },
  {
    title: 'Completion Tokens',
    key: 'completion_tokens',
    width: 140,
    render: (row: UsageStat) => formatTokens(row.completion_tokens),
  },
  {
    title: '总 Tokens',
    key: 'total_tokens',
    width: 100,
    render: (row: UsageStat) => formatTokens(row.total_tokens),
  },
  {
    title: '缓存命中率',
    key: 'cache_hit_rate',
    width: 110,
    render: (row: UsageStat) => {
      if (row.prompt_tokens === 0) return '-'
      const rate = (row.cached_tokens / row.prompt_tokens) * 100
      return `${rate.toFixed(2)}%`
    },
  },
  {
    title: '费用',
    key: 'cost_estimate',
    width: 100,
    render: (row: UsageStat) => `$${row.cost_estimate.toFixed(4)}`,
  },
]

function getDaysFromRange(range: 'today' | 'week' | 'month'): number {
  switch (range) {
    case 'today': return 1
    case 'week': return 7
    case 'month': return 30
  }
}

function buildLineChartOptions(data: UsageTrendPoint[]) {
  const theme = getEchartsTheme(isDark.value)

  if (data.length === 0) {
    return {
      backgroundColor: 'transparent',
      tooltip: { trigger: 'axis' },
      legend: { textStyle: { color: theme.text } },
      grid: { left: '3%', right: '4%', bottom: '3%', containLabel: true },
      xAxis: { type: 'category', boundaryGap: false, data: [], axisLine: { lineStyle: { color: theme.border } }, axisLabel: { color: theme.text } },
      yAxis: { type: 'value', name: 'Tokens', axisLine: { lineStyle: { color: theme.border } }, splitLine: { lineStyle: { color: theme.border } }, axisLabel: { color: theme.text }, nameTextStyle: { color: theme.text } },
      series: [],
    }
  }

  const dateSet = new Set<string>()
  const modelSet = new Set<string>()
  data.forEach(item => {
    dateSet.add(item.date)
    modelSet.add(item.model)
  })
  const dates = Array.from(dateSet).sort()
  const models = Array.from(modelSet)

  const lookup = new Map<string, number>()
  data.forEach(item => {
    lookup.set(`${item.date}|${item.model}`, item.total_tokens)
  })

  return {
    backgroundColor: 'transparent',
    tooltip: { trigger: 'axis', backgroundColor: theme.bg, borderColor: theme.border, textStyle: { color: theme.text } },
    legend: { data: models, textStyle: { color: theme.text } },
    grid: { left: '3%', right: '4%', bottom: '3%', containLabel: true },
    xAxis: {
      type: 'category',
      boundaryGap: false,
      data: dates,
      axisLine: { lineStyle: { color: theme.border } },
      axisLabel: { color: theme.text, fontSize: 11 },
    },
    yAxis: {
      type: 'value',
      name: 'Tokens',
      axisLine: { lineStyle: { color: theme.border } },
      splitLine: { lineStyle: { color: theme.border } },
      axisLabel: { color: theme.text, fontSize: 11 },
      nameTextStyle: { color: theme.text },
    },
    series: models.map((model, i) => ({
      name: model,
      type: 'line',
      smooth: true,
      symbol: 'none',
      lineStyle: { width: 2, color: theme.palette[i % theme.palette.length] },
      areaStyle: { opacity: 0.08, color: theme.palette[i % theme.palette.length] },
      data: dates.map(date => lookup.get(`${date}|${model}`) ?? 0),
    })),
  }
}

function buildPieChartOptions(data: UsageStat[]) {
  const theme = getEchartsTheme(isDark.value)
  const costData = data.map((item) => ({
    name: item.target_model || item.model,
    value: Number(item.cost_estimate.toFixed(4)),
  }))

  return {
    backgroundColor: 'transparent',
    tooltip: { trigger: 'item', formatter: '{b}: ${c} ({d}%)', backgroundColor: theme.bg, borderColor: theme.border, textStyle: { color: theme.text } },
    legend: { orient: 'vertical', left: 'left', textStyle: { color: theme.text } },
    color: theme.palette,
    series: [
      {
        type: 'pie',
        radius: ['40%', '70%'],
        avoidLabelOverlap: false,
        itemStyle: { borderRadius: 10, borderColor: theme.bg, borderWidth: 2 },
        label: { show: false, position: 'center' },
        emphasis: {
          label: { show: true, fontSize: 16, fontWeight: 'bold', color: theme.text },
        },
        labelLine: { show: false },
        data: costData,
      },
    ],
  }
}

async function fetchStats() {
  try {
    const days = getDaysFromRange(timeRange.value)
    const result = await api<UsageResponse>(`/api/usage?days=${days}`)
    stats.value = result.stats
    await nextTick()
    if (pieChart) {
      pieChart.setOption(buildPieChartOptions(stats.value))
    }
  } catch (error) {
    console.error('Failed to load usage stats:', error)
  }
}

async function fetchTrend() {
  try {
    const days = getDaysFromRange(timeRange.value)
    const data = await api<UsageTrendPoint[]>(`/api/usage/trend?days=${days}`)
    await nextTick()
    if (lineChart) {
      lineChart.setOption(buildLineChartOptions(data))
    }
  } catch (error) {
    console.error('Failed to load usage trend:', error)
  }
}

function handleRangeChange() {
  fetchStats()
  fetchTrend()
}

function handleResize() {
  lineChart?.resize()
  pieChart?.resize()
}

onMounted(async () => {
  await nextTick()
  if (lineChartRef.value) {
    lineChart = echarts.init(lineChartRef.value)
  }
  if (pieChartRef.value) {
    pieChart = echarts.init(pieChartRef.value)
  }
  fetchStats()
  fetchTrend()
  window.addEventListener('resize', handleResize)
})

onUnmounted(() => {
  window.removeEventListener('resize', handleResize)
  lineChart?.dispose()
  pieChart?.dispose()
})
</script>
