import type {
  ImageInfo,
  ListResponse,
  CleanupProgress,
  MigrationProgress,
  SiteSettings,
  StorageBackend,
  Theme,
  UploadResult,
} from './types'

export async function getSettings(): Promise<SiteSettings> {
  const r = await fetch('/api/settings')
  if (!r.ok) throw new Error('读取设置失败')
  return r.json()
}

/** 留空的密码 / 密钥不发送，表示保留原值 */
export interface ProfilePatch {
  id?: number
  name: string
  kind: StorageBackend
  webdav_url?: string
  webdav_username?: string
  webdav_password?: string
  s3_endpoint?: string
  s3_bucket?: string
  s3_region?: string
  s3_access_key?: string
  s3_secret_key?: string
  s3_path_style?: boolean
}

export async function updateSettings(patch: {
  title?: string
  theme?: Theme
  max_file_mb?: number
  max_body_mb?: number
  active_profile?: number
}): Promise<void> {
  const r = await fetch('/api/settings', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(patch),
  })
  if (!r.ok) {
    const body = await r.json().catch(() => null)
    throw new Error(body?.error ?? `保存失败 (${r.status})`)
  }
}

export async function saveProfile(profile: ProfilePatch): Promise<number> {
  const r = await fetch('/api/settings/storage/profiles', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(profile),
  })
  const body = await r.json().catch(() => null)
  if (!r.ok) throw new Error(body?.error ?? `保存失败 (${r.status})`)
  return body.id
}

export async function deleteProfile(id: number): Promise<void> {
  const r = await fetch(`/api/settings/storage/profiles?id=${id}`, { method: 'DELETE' })
  if (!r.ok) {
    const body = await r.json().catch(() => null)
    throw new Error(body?.error ?? `删除失败 (${r.status})`)
  }
}

export async function probeStorage(
  target: { id: number } | { profile: ProfilePatch },
): Promise<{ ok: boolean; message: string }> {
  const r = await fetch('/api/settings/storage/probe', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(target),
  })
  const body = await r.json().catch(() => null)
  if (!r.ok) throw new Error(body?.error ?? `测试失败 (${r.status})`)
  return body
}

export async function startMigration(
  target: number | null,
  deleteSource: boolean,
): Promise<void> {
  const r = await fetch('/api/settings/storage/migrate', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ target, delete_source: deleteSource }),
  })
  if (!r.ok) {
    const body = await r.json().catch(() => null)
    throw new Error(body?.error ?? `启动迁移失败 (${r.status})`)
  }
}

export async function cancelMigration(): Promise<void> {
  const r = await fetch('/api/settings/storage/migrate', { method: 'DELETE' })
  if (!r.ok) {
    const body = await r.json().catch(() => null)
    throw new Error(body?.error ?? `取消迁移失败 (${r.status})`)
  }
}

export async function getMigrationProgress(): Promise<{
  migration: MigrationProgress
  pending: number
}> {
  const r = await fetch('/api/settings/storage/migrate/progress')
  if (!r.ok) throw new Error(`读取迁移进度失败 (${r.status})`)
  return r.json()
}

/** 预演：后台扫描孤儿，不删任何东西。进度走 getCleanupProgress */
export async function scanOrphans(profile: number): Promise<void> {
  const r = await fetch('/api/settings/storage/orphans', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ profile }),
  })
  if (!r.ok) {
    const body = await r.json().catch(() => null)
    throw new Error(body?.error ?? `扫描失败 (${r.status})`)
  }
}

/** 真正删除；必须显式传 confirm。进度走 getCleanupProgress */
export async function deleteOrphans(profile: number): Promise<void> {
  const r = await fetch('/api/settings/storage/orphans', {
    method: 'DELETE',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ profile, confirm: true }),
  })
  if (!r.ok) {
    const body = await r.json().catch(() => null)
    throw new Error(body?.error ?? `清理失败 (${r.status})`)
  }
}

export async function getCleanupProgress(): Promise<CleanupProgress> {
  const r = await fetch('/api/settings/storage/orphans/progress')
  if (!r.ok) throw new Error(`读取清理进度失败 (${r.status})`)
  return (await r.json()).cleanup
}

export async function cancelCleanup(): Promise<void> {
  const r = await fetch('/api/settings/storage/orphans/cancel', { method: 'POST' })
  if (!r.ok) {
    const body = await r.json().catch(() => null)
    throw new Error(body?.error ?? `停止清理失败 (${r.status})`)
  }
}

export async function uploadFavicon(file: File): Promise<void> {
  const form = new FormData()
  form.append('file', file)
  const r = await fetch('/api/settings/favicon', { method: 'PUT', body: form })
  if (!r.ok) {
    const body = await r.json().catch(() => null)
    throw new Error(body?.error ?? `上传失败 (${r.status})`)
  }
}

export async function deleteFavicon(): Promise<void> {
  const r = await fetch('/api/settings/favicon', { method: 'DELETE' })
  if (!r.ok) throw new Error(`移除失败 (${r.status})`)
}

export async function listImages(limit: number, before: number | null): Promise<ListResponse> {
  const params = new URLSearchParams({ limit: String(limit) })
  if (before !== null) params.set('before', String(before))
  const r = await fetch(`/api/images?${params}`)
  if (!r.ok) throw new Error(`加载图库失败 (${r.status})`)
  return r.json()
}

export function uploadFiles(
  files: File[],
  onProgress?: (pct: number) => void,
): Promise<UploadResult[]> {
  return new Promise((resolve, reject) => {
    const form = new FormData()
    for (const f of files) form.append('files', f)

    const xhr = new XMLHttpRequest()
    xhr.open('POST', '/api/upload')
    if (onProgress) {
      xhr.upload.onprogress = (e) => {
        if (e.lengthComputable) onProgress(e.loaded / e.total)
      }
    }
    xhr.onload = () => {
      if (xhr.status === 401) {
        reject(new Error('登录已过期，请刷新页面重新登录'))
        return
      }
      if (xhr.status >= 200 && xhr.status < 300) {
        try {
          resolve(JSON.parse(xhr.responseText).results)
        } catch {
          reject(new Error('响应解析失败'))
        }
        return
      }
      try {
        reject(new Error(JSON.parse(xhr.responseText).error))
      } catch {
        reject(new Error(`上传失败 (${xhr.status})`))
      }
    }
    xhr.onerror = () => reject(new Error('网络错误，上传中断'))
    xhr.send(form)
  })
}

export async function deleteImage(id: number): Promise<void> {
  const r = await fetch(`/api/images/${id}`, { method: 'DELETE' })
  if (r.status === 401) throw new Error('登录已过期，请刷新页面重新登录')
  if (!r.ok) throw new Error(`删除失败 (${r.status})`)
}

export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text)
    return true
  } catch {
    const ta = document.createElement('textarea')
    ta.value = text
    ta.style.position = 'fixed'
    ta.style.opacity = '0'
    document.body.appendChild(ta)
    ta.select()
    const ok = document.execCommand('copy')
    ta.remove()
    return ok
  }
}

export function formatsFor(img: ImageInfo): { label: string; text: string }[] {
  // 未配置 AMOR_BASE_URL 时用浏览器当前地址拼接，配置了则后端 link 覆盖
  const base = img.link ?? window.location.origin
  const alt = img.orig_name.replace(/\.[^.]+$/, '')
  return [
    { label: '链接', text: `${base}${img.url}` },
    { label: 'Markdown', text: `![${alt}](${base}${img.url})` },
    { label: 'HTML', text: `<img src="${base}${img.url}" alt="${alt}" />` },
    { label: 'BBCode', text: `[img]${base}${img.url}[/img]` },
  ]
}
