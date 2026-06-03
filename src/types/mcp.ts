import type { AppType } from './index'

export interface McpServer {
  id: string
  name: string
  transport_type: 'stdio' | 'sse' | 'streamable-http'
  command: string | null
  args: string | null
  url: string | null
  headers: string | null
  env: string | null
  description: string | null
  created_at: string
  updated_at: string
}

export interface McpAppBinding {
  mcp_server_id: string
  app_type: AppType
  enabled: number
}

export interface McpServerWithBindings extends McpServer {
  bindings: McpAppBinding[]
}

export interface McpAppBindingInput {
  app_type: AppType
  enabled: boolean
}

export interface CreateMcpServerBody {
  name: string
  transport_type: string
  command?: string | null
  args?: string | null
  url?: string | null
  headers?: string | null
  env?: string | null
  description?: string | null
  bindings?: McpAppBindingInput[]
}

export interface UpdateMcpServerBody {
  name?: string
  transport_type?: string
  command?: string | null
  args?: string | null
  url?: string | null
  headers?: string | null
  env?: string | null
  description?: string | null
}

export interface ImportResult {
  imported: number
  skipped: number
}

export interface ApplyResult {
  applied: number
}
