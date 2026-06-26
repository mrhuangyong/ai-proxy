export interface VirtualModelMapping {
  id: string
  virtual_model_id: string
  provider_id: string
  provider_model_id: string
  label: string // "zai/glm-5.2"
  priority: number
  enabled: boolean
  available: boolean
  consecutive_failures: number
  failover_count: number
  last_failure_at: string | null
  last_checked_at: string | null
  created_at: string
  is_current: boolean // == virtual_model.current_mapping_id
}

export interface VirtualModel {
  id: string
  name: string
  description: string | null
  current_mapping_id: string | null
  enabled: boolean
  mappings: VirtualModelMapping[]
}

export interface RealModelOption {
  provider_model_id: string
  provider_id: string
  label: string // "zai/glm-5.2"
}

export interface CreateMappingInput {
  provider_model_id: string
  priority?: number
  enabled?: boolean
}

export interface CreateVirtualModelBody {
  name: string
  description?: string
  enabled?: boolean
  mappings: CreateMappingInput[]
}

export interface UpdateVirtualModelBody {
  name?: string
  description?: string
  enabled?: boolean
}

export interface UpdateMappingBody {
  priority?: number
  enabled?: boolean
}

export interface SetAvailableBody {
  available: boolean
}

export interface SetStickyBody {
  mapping_id: string | null
}