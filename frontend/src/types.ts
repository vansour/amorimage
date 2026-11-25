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

/** 密钥只回传「是否已设置」，不回传值 */
export interface StorageSettings {
  backend: StorageBackend
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
