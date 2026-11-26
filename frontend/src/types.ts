export interface ImageInfo {
  id: number
  hash: string
  orig_name: string
  ext: string
  mime: string
  width: number
  height: number
  size: number
  thumb_size: number
  created_at: number
  url: string
  thumb_url: string
  link?: string
}

export interface UploadResult {
  name: string
  status: 'ok' | 'duplicate' | 'error'
  message?: string
  image?: ImageInfo
}

export interface ListResponse {
  total: number
  items: ImageInfo[]
}

export type Theme = 'dark' | 'light'

export type StorageBackend = 'local' | 'webdav' | 's3'

/** 一条命名的存储配置；密钥只回传「是否已设置」，不回传值 */
export interface StorageProfile {
  id: number
  name: string
  kind: StorageBackend
  /** 该档案上的图片数量 */
  images: number
  webdav_url: string
  webdav_username: string
  webdav_has_password: boolean
  s3_endpoint: string
  s3_bucket: string
  s3_region: string
  s3_access_key: string
  s3_has_secret: boolean
  s3_path_style: boolean
}

export interface MigrationProgress {
  running: boolean
  /** 是否跑过至少一次 */
  present: boolean
  target: number
  total: number
  done: number
  moved: number
  skipped: number
  failed: number
  cancelled: boolean
  errors: string[]
}

/** 孤儿文件清理的后台任务进度 */
export interface CleanupProgress {
  running: boolean
  /** 是否跑过至少一次 */
  present: boolean
  profile: number
  /** 'scan' 只统计，'delete' 在删除 */
  phase: 'scan' | 'delete'
  /** 扫描阶段：已扫文件数；删除阶段：已处理待删文件数 */
  total: number
  done: number
  /** 扫描到的文件总数，删除阶段仍保留 */
  scanned: number
  orphans: number
  kept: number
  deleted: number
  failed: number
  cancelled: boolean
  errors: string[]
  /** 部分孤儿文件名（最多 200 条） */
  sample: string[]
}

export interface StorageSettings {
  /** 当前使用的档案 id */
  active: number
  profiles: StorageProfile[]
  migration: MigrationProgress
  /** 不在当前档案、等待迁移的图片数 */
  pending: number
}

export interface SiteSettings {
  title: string
  theme: Theme
  /** null 表示无限制 */
  max_file_mb: number | null
  /** null 表示无限制 */
  max_body_mb: number | null
  has_favicon: boolean
  storage: StorageSettings
}
