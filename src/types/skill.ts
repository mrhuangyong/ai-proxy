export interface SkillSource {
  id: string
  name: string
  path: string
  is_global: boolean
  is_default: boolean
  discovery_order: number
  created_at: string
  updated_at: string
}

export interface SkillSourceWithCount {
  id: string
  name: string
  path: string
  is_global: boolean
  is_default: boolean
  discovery_order: number
  created_at: string
  updated_at: string
  skill_count: number
}

export interface Skill {
  id: string
  name: string
  description: string
  source_id: string
  skill_path: string
  is_symlink: boolean
  link_target: string | null
  has_skill_md: boolean
  created_at: string
  updated_at: string
}

export interface SkillDetail {
  id: string
  name: string
  description: string
  source_id: string
  skill_path: string
  is_symlink: boolean
  link_target: string | null
  has_skill_md: boolean
  created_at: string
  updated_at: string
  skill_md_content: string | null
}

export interface CreateSkillSourceBody {
  name: string
  path: string
}

export interface CreateSkillBody {
  name: string
  description?: string
  skill_md_content?: string
}

export interface UpdateSkillMdBody {
  content: string
}

export interface InstallSkillBody {
  skill_id: string
  target_source_ids: string[]
}

export interface UninstallSkillBody {
  skill_id: string
}

export interface InstallFromUrlBody {
  url: string
}
