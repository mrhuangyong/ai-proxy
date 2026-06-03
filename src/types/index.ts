export interface Provider {
  id: string
  name: string
  base_url: string
  format: 'completions' | 'responses' | 'anthropic' | 'gemini'
  endpoint_path?: string | null
  models: ProviderModel[]
  api_keys: ApiKeyInfo[]
}

export interface ProviderModel {
  id: string
  provider_id: string
  model_name: string
  target_model: string | null
  enabled: boolean
  created_at: string
}

export interface ApiKeyInfo {
  id: string
  label: string
  is_active: boolean
  usage_count: number
  last_used_at: string | null
  created_at: string
}

export interface RequestLog {
  id: number
  request_id: string
  client_format: string
  provider_name: string
  provider_format: string
  model: string
  stream: boolean
  status_code: number | null
  duration_ms: number | null
  prompt_tokens: number
  completion_tokens: number
  total_tokens: number
  cached_tokens: number
  ttft_ms: number | null
  error_message: string | null
  created_at: string
}

export interface UsageSummary {
  model: string
  provider_name: string
  total_prompt_tokens: number
  total_completion_tokens: number
  total_tokens: number
  total_cost: number
  request_count: number
}

export interface UsageTrendPoint {
  date: string
  model: string
  prompt_tokens: number
  completion_tokens: number
  total_tokens: number
}

export interface InterceptorRule {
  id: string
  name: string
  phase: 'pre' | 'post'
  condition: RuleCondition
  action: RuleAction
  priority: number
  enabled: boolean
}

export type RuleCondition =
  | { type: 'model_matches'; pattern: string }
  | { type: 'path_contains'; substring: string }
  | { type: 'header_exists'; name: string }
  | { type: 'always' }

export type RuleAction =
  | { type: 'replace_model'; model: string }
  | { type: 'set_header'; name: string; value: string }
  | { type: 'remove_header'; name: string }
  | { type: 'inject_system_prompt'; prompt: string }
  | { type: 'override_parameter'; parameter: string; value: unknown }
  | { type: 'filter_response'; patterns: string[] }

export type AppType = 'codex_cli' | 'codex_desktop' | 'claude_cli' | 'claude_desktop'

export interface AppConfig {
  app_type: AppType
  installed: boolean
  install_path: string | null
  config_path: string | null
  model: string | null
  model_haiku: string | null
  model_sonnet: string | null
  model_opus: string | null
  work_dir: string | null
  proxy_url: string | null
  launched_at: string | null
  status: 'success' | 'config_error' | 'launch_error' | null
}

export interface LaunchRequest {
  app_type: AppType
  model: string
  model_haiku?: string
  model_sonnet?: string
  model_opus?: string
  work_dir?: string
}

export interface SetPathRequest {
  install_path: string
}

export type { McpServer, McpAppBinding, McpServerWithBindings, McpAppBindingInput, CreateMcpServerBody, UpdateMcpServerBody, ImportResult, ApplyResult } from './mcp'
