export type ProviderFormat = 'completions' | 'responses' | 'anthropic' | 'gemini'

/** One upstream protocol a provider speaks (base_url/endpoint optionally override the provider defaults). */
export interface ProviderProtocol {
  format: ProviderFormat
  /** Empty/null → use the provider-level base_url. */
  base_url?: string | null
  /** Empty/null → the default path for the format. */
  endpoint_path?: string | null
  /** Primary protocol = conversion target when the client protocol has no match. */
  is_primary: boolean
}

export interface Provider {
  id: string
  name: string
  base_url: string
  format: ProviderFormat
  endpoint_path?: string | null
  upstream_user_agent?: string
  enabled: boolean
  models: ProviderModel[]
  api_keys: ApiKeyInfo[]
  protocols: ProviderProtocol[]
}

export interface ModelCapabilities {
  supports_thinking: boolean
  supports_tools: boolean
  supports_temperature: boolean
  supports_top_p: boolean
  supports_top_k: boolean
  supports_presence_penalty: boolean
  supports_frequency_penalty: boolean
  supports_seed: boolean
  supports_response_format: boolean
  supports_stream_options: boolean
  supports_stop: boolean
  /** Null = do not clamp. */
  max_output_tokens: number | null
  /** When false, the unguarded `extra` passthrough is cleared. */
  extra_passthrough: boolean
}

export interface ProviderModel {
  id: string
  provider_id: string
  model_name: string
  target_model: string | null
  context_window: number
  enabled: boolean
  created_at: string
  capabilities: ModelCapabilities
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
  target_model: string
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
  final_usage_json?: string | null
  upstream_usage_events_json?: string | null
  client_user_agent?: string | null
  is_passthrough?: boolean
}

export interface UsageSummary {
  model: string
  target_model: string
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

export type AppType = 'codex_cli' | 'codex_desktop' | 'claude_cli' | 'claude_desktop' | 'opencode_cli'

export interface AppConfig {
  app_type: AppType
  installed: boolean
  install_path: string | null
  config_path: string | null
  model: string | null
  model_haiku: string | null
  model_sonnet: string | null
  model_opus: string | null
  opencode_models: string[] | null
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
  models?: string[]
  work_dir?: string
}

export interface SetPathRequest {
  install_path: string
}

export type { McpServer, McpAppBinding, McpServerWithBindings, McpAppBindingInput, CreateMcpServerBody, UpdateMcpServerBody, ImportResult, ApplyResult } from './mcp'

export type {
  SkillSource,
  SkillSourceWithCount,
  Skill,
  SkillDetail,
  CreateSkillSourceBody,
  CreateSkillBody,
  UpdateSkillMdBody,
  InstallSkillBody,
  UninstallSkillBody,
  InstallFromUrlBody,
} from './skill'

export type {
  VirtualModel,
  VirtualModelMapping,
  RealModelOption,
  CreateMappingInput,
  CreateVirtualModelBody,
  UpdateVirtualModelBody,
  UpdateMappingBody,
  SetAvailableBody,
  SetStickyBody,
} from './virtualModel'
