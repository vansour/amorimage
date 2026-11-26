<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import {
  cancelCleanup,
  cancelMigration,
  deleteFavicon,
  deleteOrphans,
  deleteProfile,
  getCleanupProgress,
  getMigrationProgress,
  getSettings,
  probeStorage,
  saveProfile,
  scanOrphans,
  startMigration,
  updateSettings,
  uploadFavicon,
} from '../api'
import type {
  CleanupProgress,
  MigrationProgress,
  SiteSettings,
  StorageBackend,
  StorageProfile,
  Theme,
} from '../types'

type SectionKey = 'site' | 'upload' | 'icon' | 'theme' | 'storage'

const menus: { key: SectionKey; label: string }[] = [
  { key: 'site', label: '站点' },
  { key: 'upload', label: '图片' },
  { key: 'icon', label: '图标' },
  { key: 'theme', label: '主题' },
  { key: 'storage', label: '存储' },
]

const emit = defineEmits<{ changed: [s: SiteSettings] }>()

const active = ref<SectionKey>('site')
const title = ref('')
const maxFileMb = ref<number | null>(null)
const maxBodyMb = ref<number | null>(null)
const theme = ref<Theme>('dark')
const hasFavicon = ref(false)
const faviconVersion = ref(0)
const titleStatus = ref('')
const fileStatus = ref('')
const bodyStatus = ref('')
const iconStatus = ref('')
const themeStatus = ref('')
const savingTitle = ref(false)
const savingFile = ref(false)
const savingBody = ref(false)
const iconFile = ref<File | null>(null)

const profiles = ref<StorageProfile[]>([])
const activeProfile = ref(0)
const pendingMove = ref(0)
const migration = ref<MigrationProgress | null>(null)

const editing = ref(false)
const editId = ref<number | null>(null)
const editName = ref('')
const editKind = ref<StorageBackend>('local')
const webdavUrl = ref('')
const webdavUsername = ref('')
const webdavPassword = ref('')
const webdavHasPassword = ref(false)
const s3Endpoint = ref('')
const s3Bucket = ref('')
const s3Region = ref('')
const s3AccessKey = ref('')
const s3SecretKey = ref('')
const s3HasSecret = ref(false)
const s3PathStyle = ref(true)
const storageStatus = ref('')
const storageSaving = ref(false)
const storageProbing = ref(false)
const deleteSource = ref(false)
const migrating = ref(false)

/** 清理目标档案与后台任务进度；confirming 表示已经预演过、等待二次确认 */
const cleanupProfile = ref(0)
const cleanupProgress = ref<CleanupProgress | null>(null)
const cleanupConfirming = ref(false)
const cleanupBusy = ref(false)
const cleanupStatus = ref('')

const cleanupPct = computed(() => {
  const c = cleanupProgress.value
  if (!c || c.total === 0) return 0
  return Math.min(100, Math.round((c.done / c.total) * 100))
})

/** 扫描完成后才允许删除：只有预演过才知道会删哪些 */
const cleanupReadyToDelete = computed(() => {
  const c = cleanupProgress.value
  return !!c && !c.running && c.phase === 'scan' && c.orphans > 0
})

/** 本轮是否已经删过：删完就不再提示「有 N 个孤儿待删」 */
const cleanupFinishedDelete = computed(() => {
  const c = cleanupProgress.value
  return !!c && !c.running && c.phase === 'delete'
})

const activeName = computed(
  () => profiles.value.find((p) => p.id === activeProfile.value)?.name ?? '未选择',
)

const pct = computed(() => {
  const m = migration.value
  if (!m || m.total === 0) return 0
  return Math.min(100, Math.round((m.done / m.total) * 100))
})

let pollTimer: number | undefined
let cleanupTimer: number | undefined

async function load() {
  const s = await getSettings()
  title.value = s.title
  maxFileMb.value = s.max_file_mb
  maxBodyMb.value = s.max_body_mb
  theme.value = s.theme
  hasFavicon.value = s.has_favicon
  profiles.value = s.storage.profiles
  activeProfile.value = s.storage.active
  pendingMove.value = s.storage.pending
  migration.value = s.storage.migration.running || s.storage.migration.present
    ? s.storage.migration
    : null
  // 清理目标默认选第一个非当前档案：当前档案上的孤儿通常刚被迁移清空
  if (!profiles.value.some((p) => p.id === cleanupProfile.value)) {
    const other = profiles.value.find((p) => p.id !== activeProfile.value)
    cleanupProfile.value = (other ?? profiles.value[0])?.id ?? 0
  }
  emit('changed', s)
  syncPoll()
  syncCleanupPoll()
}

/** 进页面时补一次清理状态：可能是从这里离开后又切回来的 */
async function loadCleanupState() {
  try {
    const p = await getCleanupProgress()
    cleanupProgress.value = p.present || p.running ? p : null
    syncCleanupPoll()
  } catch {
    // 状态读不到不影响页面本身
  }
}

/** 迁移进行中才轮询；页面卸载时停掉，避免后台空转 */
function syncPoll() {
  const running = !!migration.value?.running
  if (running && pollTimer === undefined) {
    pollTimer = window.setInterval(async () => {
      try {
        const p = await getMigrationProgress()
        migration.value = p.migration
        pendingMove.value = p.pending
        if (!p.migration.running) {
          window.clearInterval(pollTimer)
          pollTimer = undefined
          await load()
        }
      } catch {
        window.clearInterval(pollTimer)
        pollTimer = undefined
      }
    }, 1000)
  } else if (!running && pollTimer !== undefined) {
    window.clearInterval(pollTimer)
    pollTimer = undefined
  }
}

/** 清理任务同样只在跑的时候轮询 */
function syncCleanupPoll() {
  const running = !!cleanupProgress.value?.running
  if (running && cleanupTimer === undefined) {
    cleanupTimer = window.setInterval(async () => {
      try {
        const p = await getCleanupProgress()
        cleanupProgress.value = p
        if (!p.running) {
          window.clearInterval(cleanupTimer)
          cleanupTimer = undefined
          await load()
        }
      } catch {
        window.clearInterval(cleanupTimer)
        cleanupTimer = undefined
      }
    }, 700)
  } else if (!running && cleanupTimer !== undefined) {
    window.clearInterval(cleanupTimer)
    cleanupTimer = undefined
  }
}

function resetEdit() {
  editing.value = false
  editId.value = null
  editName.value = ''
  editKind.value = 'local'
  webdavUrl.value = ''
  webdavUsername.value = ''
  webdavPassword.value = ''
  webdavHasPassword.value = false
  s3Endpoint.value = ''
  s3Bucket.value = ''
  s3Region.value = ''
  s3AccessKey.value = ''
  s3SecretKey.value = ''
  s3HasSecret.value = false
  s3PathStyle.value = true
  storageStatus.value = ''
}

function startCreate() {
  resetEdit()
  editing.value = true
}

function startEdit(p: StorageProfile) {
  resetEdit()
  editing.value = true
  editId.value = p.id
  editName.value = p.name
  editKind.value = p.kind
  webdavUrl.value = p.webdav_url
  webdavUsername.value = p.webdav_username
  webdavHasPassword.value = p.webdav_has_password
  s3Endpoint.value = p.s3_endpoint
  s3Bucket.value = p.s3_bucket
  s3Region.value = p.s3_region
  s3AccessKey.value = p.s3_access_key
  s3HasSecret.value = p.s3_has_secret
  s3PathStyle.value = p.s3_path_style
}

/** 密码/密钥留空即保留原值；名称与地址总是发送 */
function profilePatch() {
  const patch: Parameters<typeof saveProfile>[0] = {
    name: editName.value.trim(),
    kind: editKind.value,
  }
  if (editId.value !== null) patch.id = editId.value
  if (editKind.value === 'webdav') {
    patch.webdav_url = webdavUrl.value.trim()
    patch.webdav_username = webdavUsername.value.trim()
    if (webdavPassword.value) patch.webdav_password = webdavPassword.value
  } else if (editKind.value === 's3') {
    patch.s3_endpoint = s3Endpoint.value.trim()
    patch.s3_bucket = s3Bucket.value.trim()
    patch.s3_region = s3Region.value.trim()
    patch.s3_access_key = s3AccessKey.value.trim()
    if (s3SecretKey.value) patch.s3_secret_key = s3SecretKey.value
    patch.s3_path_style = s3PathStyle.value
  }
  return patch
}

async function saveStorage() {
  storageSaving.value = true
  storageStatus.value = ''
  try {
    await saveProfile(profilePatch())
    resetEdit()
    await load()
  } catch (e) {
    storageStatus.value = (e as Error).message
  } finally {
    storageSaving.value = false
  }
}

async function testStorage() {
  storageProbing.value = true
  storageStatus.value = ''
  try {
    const r = await probeStorage(
      editId.value !== null && !editName.value.trim()
        ? { id: editId.value }
        : { profile: profilePatch() },
    )
    storageStatus.value = r.message
  } catch (e) {
    storageStatus.value = (e as Error).message
  } finally {
    storageProbing.value = false
  }
}

async function switchActive(id: number) {
  if (id === activeProfile.value) return
  storageStatus.value = ''
  try {
    await updateSettings({ active_profile: id })
    await load()
  } catch (e) {
    storageStatus.value = (e as Error).message
  }
}

async function removeProfile(p: StorageProfile) {
  storageStatus.value = ''
  try {
    await deleteProfile(p.id)
    if (editId.value === p.id) resetEdit()
    await load()
  } catch (e) {
    storageStatus.value = (e as Error).message
  }
}

async function runMigration() {
  migrating.value = true
  storageStatus.value = ''
  try {
    await startMigration(activeProfile.value, deleteSource.value)
    const p = await getMigrationProgress()
    migration.value = p.migration
    pendingMove.value = p.pending
    syncPoll()
  } catch (e) {
    storageStatus.value = (e as Error).message
  } finally {
    migrating.value = false
  }
}

async function stopMigration() {
  try {
    await cancelMigration()
  } catch (e) {
    storageStatus.value = (e as Error).message
  }
}

function passwordHint(has: boolean): string {
  return has ? '留空则保持现有密码' : ''
}

/** 切换清理目标时作废上一次的预演结果，避免拿旧清单去删别的档案 */
function pickCleanupTarget(id: number) {
  cleanupProfile.value = id
  cleanupProgress.value = null
  cleanupConfirming.value = false
  cleanupStatus.value = ''
}

async function scanCleanup() {
  cleanupBusy.value = true
  cleanupStatus.value = ''
  cleanupConfirming.value = false
  try {
    await scanOrphans(cleanupProfile.value)
    await refreshCleanup()
    syncCleanupPoll()
  } catch (e) {
    cleanupStatus.value = (e as Error).message
  } finally {
    cleanupBusy.value = false
  }
}

async function runCleanup() {
  cleanupBusy.value = true
  cleanupStatus.value = ''
  cleanupConfirming.value = false
  try {
    await deleteOrphans(cleanupProfile.value)
    await refreshCleanup()
    syncCleanupPoll()
  } catch (e) {
    cleanupStatus.value = (e as Error).message
  } finally {
    cleanupBusy.value = false
  }
}

async function refreshCleanup() {
  try {
    cleanupProgress.value = await getCleanupProgress()
  } catch (e) {
    cleanupStatus.value = (e as Error).message
  }
}

async function stopCleanup() {
  try {
    await cancelCleanup()
  } catch (e) {
    cleanupStatus.value = (e as Error).message
  }
}

function kindLabel(kind: StorageBackend): string {
  return kind === 'local' ? '本地磁盘' : kind === 'webdav' ? 'WebDAV' : 'S3'
}

async function saveTitle() {
  savingTitle.value = true
  titleStatus.value = ''
  try {
    await updateSettings({ title: title.value })
    await load()
    titleStatus.value = '已保存'
  } catch (e) {
    titleStatus.value = (e as Error).message
  } finally {
    savingTitle.value = false
  }
}

function normLimit(v: number | null): number {
  const n = Number(v)
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : 0
}

async function saveMaxFileMb() {
  savingFile.value = true
  fileStatus.value = ''
  try {
    await updateSettings({ max_file_mb: normLimit(maxFileMb.value) })
    await load()
    fileStatus.value = '已保存'
  } catch (e) {
    fileStatus.value = (e as Error).message
  } finally {
    savingFile.value = false
  }
}

async function saveMaxBodyMb() {
  savingBody.value = true
  bodyStatus.value = ''
  try {
    await updateSettings({ max_body_mb: normLimit(maxBodyMb.value) })
    await load()
    bodyStatus.value = '已保存'
  } catch (e) {
    bodyStatus.value = (e as Error).message
  } finally {
    savingBody.value = false
  }
}

async function setTheme(t: Theme) {
  if (theme.value === t) return
  theme.value = t
  themeStatus.value = ''
  try {
    await updateSettings({ theme: t })
    await load()
  } catch (e) {
    theme.value = t === 'dark' ? 'light' : 'dark'
    themeStatus.value = (e as Error).message
  }
}

function pickIcon(e: Event) {
  iconFile.value = (e.target as HTMLInputElement).files?.[0] ?? null
  iconStatus.value = ''
}

async function uploadIcon() {
  if (!iconFile.value) {
    iconStatus.value = '先选择一个文件'
    return
  }
  iconStatus.value = ''
  try {
    await uploadFavicon(iconFile.value)
    faviconVersion.value++
    await load()
    iconStatus.value = '已更新'
  } catch (e) {
    iconStatus.value = (e as Error).message
  }
}

async function removeIcon() {
  iconStatus.value = ''
  try {
    await deleteFavicon()
    faviconVersion.value++
    await load()
    iconStatus.value = '已移除'
  } catch (e) {
    iconStatus.value = (e as Error).message
  }
}

onMounted(async () => {
  await load()
  await loadCleanupState()
})
onUnmounted(() => {
  if (pollTimer !== undefined) window.clearInterval(pollTimer)
  if (cleanupTimer !== undefined) window.clearInterval(cleanupTimer)
})
</script>

<template>
  <section class="settings" aria-label="设置">
    <h2>设置</h2>
    <div class="settings-layout">
      <nav class="settings-nav">
        <button
          v-for="m in menus"
          :key="m.key"
          :class="{ on: active === m.key }"
          @click="active = m.key"
        >
          {{ m.label }}
        </button>
      </nav>

      <div class="settings-content">
        <div v-if="active === 'site'" class="panel">
          <header class="panel-head">
            <h3>站点标题</h3>
            <p>显示在页头与浏览器标签页，登录页同步。留空恢复默认 amorimage。</p>
          </header>
          <div class="panel-body">
            <div class="form-row">
              <input
                id="site-title"
                v-model="title"
                type="text"
                maxlength="60"
                placeholder="amorimage"
                @keydown.enter="saveTitle"
              />
              <button :disabled="savingTitle" @click="saveTitle">保存</button>
              <span v-if="titleStatus" class="set-status">{{ titleStatus }}</span>
            </div>
          </div>
        </div>

        <div v-else-if="active === 'upload'" class="panel">
          <header class="panel-head">
            <h3>上传限制</h3>
            <p>修改立即生效；填 0 或留空表示无限制。</p>
          </header>
          <div class="panel-body">
            <div class="form-row">
              <label class="row-label" for="max-file-mb">单张图片上限</label>
              <input
                id="max-file-mb"
                v-model.number="maxFileMb"
                type="number"
                min="0"
                step="1"
                placeholder="无限制"
                @keydown.enter="saveMaxFileMb"
              />
              <span class="set-hint">MB</span>
              <button :disabled="savingFile" @click="saveMaxFileMb">保存</button>
              <span v-if="fileStatus" class="set-status">{{ fileStatus }}</span>
            </div>
            <div class="form-row">
              <label class="row-label" for="max-body-mb">请求总量上限</label>
              <input
                id="max-body-mb"
                v-model.number="maxBodyMb"
                type="number"
                min="0"
                step="1"
                placeholder="无限制"
                @keydown.enter="saveMaxBodyMb"
              />
              <span class="set-hint">MB</span>
              <button :disabled="savingBody" @click="saveMaxBodyMb">保存</button>
              <span v-if="bodyStatus" class="set-status">{{ bodyStatus }}</span>
            </div>
          </div>
        </div>

        <div v-else-if="active === 'icon'" class="panel">
          <header class="panel-head">
            <h3>站点图标</h3>
            <p>浏览器标签页显示的图标，支持 PNG / JPEG / GIF / WebP / ICO，10MB 以内。</p>
          </header>
          <div class="panel-body">
            <div class="form-row">
              <img
                v-if="hasFavicon"
                class="favicon-preview"
                :src="`/favicon.ico?v=${faviconVersion}`"
                alt="当前图标"
              />
              <span v-else class="set-hint">未设置</span>
              <input
                type="file"
                accept="image/png,image/jpeg,image/gif,image/webp,image/x-icon"
                @change="pickIcon"
              />
              <button @click="uploadIcon">上传</button>
              <button v-if="hasFavicon" class="quiet" @click="removeIcon">移除</button>
              <span v-if="iconStatus" class="set-status">{{ iconStatus }}</span>
            </div>
          </div>
        </div>

        <div v-else-if="active === 'theme'" class="panel">
          <header class="panel-head">
            <h3>页面主题</h3>
            <p>全站生效，登录页同步。</p>
          </header>
          <div class="panel-body">
            <div class="form-row">
              <div class="segmented">
                <button :class="{ on: theme === 'dark' }" @click="setTheme('dark')">暗色</button>
                <button :class="{ on: theme === 'light' }" @click="setTheme('light')">亮色</button>
              </div>
              <span v-if="themeStatus" class="set-status">{{ themeStatus }}</span>
            </div>
          </div>
        </div>

        <div v-else class="panel">
          <header class="panel-head">
            <h3>存储位置</h3>
            <p>
              每条档案保存一套独立配置。新上传写入「当前档案」，旧档案上的图片仍可正常访问；
              切换后用下方「迁移」把旧图搬到新档案。
            </p>
          </header>
          <div class="panel-body">
            <div class="form-row profile-list">
              <label class="row-label">档案</label>
              <div class="profile-items">
                <div
                  v-for="p in profiles"
                  :key="p.id"
                  class="profile-item"
                  :class="{ on: p.id === activeProfile }"
                >
                  <button class="profile-pick" @click="switchActive(p.id)">
                    <span class="profile-dot" :class="{ on: p.id === activeProfile }"></span>
                    <span class="profile-name">{{ p.name }}</span>
                    <span v-if="kindLabel(p.kind) !== p.name" class="profile-kind">
                      {{ kindLabel(p.kind) }}
                    </span>
                    <span class="profile-count">{{ p.images }} 张</span>
                    <span v-if="p.id === activeProfile" class="profile-badge">当前</span>
                  </button>
                  <span class="profile-actions">
                    <button class="mini quiet" @click="startEdit(p)">编辑</button>
                    <button
                      v-if="p.id !== activeProfile"
                      class="mini quiet"
                      :disabled="p.images > 0"
                      :title="p.images > 0 ? '该档案上还有图片，先迁移走再删除' : ''"
                      @click="removeProfile(p)"
                    >
                      删除
                    </button>
                  </span>
                </div>
              </div>
              <button class="quiet" @click="startCreate">新建档案</button>
            </div>

            <template v-if="editing">
              <div class="form-row">
                <label class="row-label" for="profile-name">名称</label>
                <input
                  id="profile-name"
                  v-model="editName"
                  type="text"
                  maxlength="40"
                  placeholder="例如：家用 NAS"
                />
              </div>
              <div class="form-row">
                <label class="row-label">类型</label>
                <div class="segmented">
                  <button
                    :class="{ on: editKind === 'local' }"
                    :disabled="editId !== null"
                    @click="editKind = 'local'"
                  >
                    本地磁盘
                  </button>
                  <button
                    :class="{ on: editKind === 'webdav' }"
                    :disabled="editId !== null"
                    @click="editKind = 'webdav'"
                  >
                    WebDAV
                  </button>
                  <button
                    :class="{ on: editKind === 's3' }"
                    :disabled="editId !== null"
                    @click="editKind = 's3'"
                  >
                    S3
                  </button>
                </div>
                <span v-if="editId !== null" class="set-hint">已有档案不能改类型</span>
              </div>

              <template v-if="editKind === 'webdav'">
                <div class="form-row">
                  <label class="row-label" for="webdav-url">地址</label>
                  <input
                    id="webdav-url"
                    v-model="webdavUrl"
                    type="text"
                    placeholder="https://dav.example.com/remote.php/dav/files/me/amorimage"
                  />
                </div>
                <div class="form-row">
                  <label class="row-label" for="webdav-user">用户名</label>
                  <input id="webdav-user" v-model="webdavUsername" type="text" autocomplete="off" />
                  <input
                    v-model="webdavPassword"
                    type="password"
                    autocomplete="new-password"
                    :placeholder="passwordHint(webdavHasPassword) || '密码'"
                  />
                </div>
              </template>

              <template v-else-if="editKind === 's3'">
                <div class="form-row">
                  <label class="row-label" for="s3-endpoint">Endpoint</label>
                  <input
                    id="s3-endpoint"
                    v-model="s3Endpoint"
                    type="text"
                    placeholder="https://s3.us-east-1.amazonaws.com"
                  />
                </div>
                <div class="form-row">
                  <label class="row-label" for="s3-bucket">存储桶</label>
                  <input id="s3-bucket" v-model="s3Bucket" type="text" placeholder="my-bucket" />
                  <label class="row-label" for="s3-region">区域</label>
                  <input id="s3-region" v-model="s3Region" type="text" placeholder="us-east-1" />
                </div>
                <div class="form-row">
                  <label class="row-label" for="s3-key">Access Key</label>
                  <input id="s3-key" v-model="s3AccessKey" type="text" autocomplete="off" />
                  <input
                    v-model="s3SecretKey"
                    type="password"
                    autocomplete="new-password"
                    :placeholder="passwordHint(s3HasSecret) || 'Secret Key'"
                  />
                </div>
                <div class="form-row">
                  <label class="row-label">URL 风格</label>
                  <div class="segmented">
                    <button :class="{ on: s3PathStyle }" @click="s3PathStyle = true">
                      Path
                    </button>
                    <button :class="{ on: !s3PathStyle }" @click="s3PathStyle = false">
                      Virtual Host
                    </button>
                  </div>
                  <span class="set-hint">MinIO 等自建服务通常用 Path</span>
                </div>
              </template>

              <div class="form-row">
                <button :disabled="storageSaving" @click="saveStorage">保存</button>
                <button :disabled="storageProbing" class="quiet" @click="testStorage">
                  {{ storageProbing ? '正在测试' : '测试连接' }}
                </button>
                <button class="quiet" @click="resetEdit">取消</button>
                <span v-if="storageStatus" class="set-status">{{ storageStatus }}</span>
              </div>
            </template>

            <div v-else class="form-row">
              <span v-if="storageStatus" class="set-status">{{ storageStatus }}</span>
            </div>

            <div class="form-row migrate-row">
              <label class="row-label">迁移</label>
              <div class="migrate-body">
                <p class="set-hint">
                  把其他档案上的图片全部搬到「{{ activeName }}」。文件复制成功后才切换记录指向，
                  原文件默认保留。
                </p>

                <template v-if="migration?.running">
                  <div class="progress">
                    <div class="progress-bar" :style="{ width: pct + '%' }"></div>
                  </div>
                  <p class="set-hint">
                    {{ migration.done }} / {{ migration.total }}（{{ pct }}%）· 已搬
                    {{ migration.moved }} · 失败 {{ migration.failed }}
                  </p>
                  <button class="quiet" @click="stopMigration">停止</button>
                </template>

                <template v-else>
                  <p v-if="pendingMove > 0" class="set-hint">
                    有 {{ pendingMove }} 张图片不在当前档案上。
                  </p>
                  <p v-else class="set-hint">所有图片都在当前档案上，无需迁移。</p>
                  <div class="migrate-actions">
                    <button :disabled="pendingMove === 0 || migrating" @click="runMigration">
                      {{ migrating ? '正在启动' : '迁移到当前档案' }}
                    </button>
                    <label class="check" title="只删除本次迁移涉及的源文件；之前分批迁移留下的源文件不受影响">
                      <input v-model="deleteSource" type="checkbox" />
                      <span>搬迁成功后删除本次的源文件</span>
                    </label>
                  </div>
                  <template v-if="migration && !migration.running && migration.present">
                    <p class="set-hint">
                      上次迁移：已完成 {{ migration.done }}，失败 {{ migration.failed }}
                      <span v-if="migration.cancelled">，已手动停止</span>
                    </p>
                    <ul v-if="migration.errors.length" class="migrate-errors">
                      <li v-for="(err, i) in migration.errors" :key="i">{{ err }}</li>
                    </ul>
                  </template>
                </template>
              </div>
            </div>

            <div class="form-row migrate-row">
              <label class="row-label">清理</label>
              <div class="migrate-body">
                <p class="set-hint">
                  删掉某个档案上「数据库里已没有记录指向」的孤儿文件。迁移后旧档案上
                  留下的副本就属于这种情况。未迁移的图片不会被删除。
                </p>
                <div class="migrate-actions">
                  <select
                    v-model.number="cleanupProfile"
                    class="cleanup-select"
                    @change="pickCleanupTarget(cleanupProfile)"
                  >
                    <option v-for="p in profiles" :key="p.id" :value="p.id">
                      {{ p.name }}（{{ p.images }} 张）
                    </option>
                  </select>
                  <button :disabled="cleanupBusy" @click="scanCleanup">
                    {{ cleanupBusy ? '扫描中' : '扫描孤儿' }}
                  </button>
                </div>

                <template v-if="cleanupProgress">
                  <template v-if="cleanupProgress.running">
                    <div class="progress">
                      <div class="progress-bar" :style="{ width: cleanupPct + '%' }"></div>
                    </div>
                    <p class="set-hint">
                      <template v-if="cleanupProgress.phase === 'scan'">
                        正在扫描 {{ cleanupProgress.done }} 个文件 · 已发现孤儿
                        {{ cleanupProgress.orphans }} 个
                      </template>
                      <template v-else>
                        正在删除 {{ cleanupProgress.done }} / {{ cleanupProgress.total }}（{{
                          cleanupPct
                        }}%）· 已删 {{ cleanupProgress.deleted }} · 失败
                        {{ cleanupProgress.failed }}
                      </template>
                    </p>
                    <button class="quiet" @click="stopCleanup">停止</button>
                  </template>

                  <template v-else>
                    <p class="set-hint">
                      该档案共 {{ cleanupProgress.scanned }} 个文件：孤儿
                      {{ cleanupProgress.orphans }} 个，仍被引用
                      {{ cleanupProgress.kept }} 个。
                    </p>
                    <p v-if="cleanupFinishedDelete" class="set-hint">
                      本轮已删除 {{ cleanupProgress.deleted }} 个
                      <span v-if="cleanupProgress.orphans > cleanupProgress.deleted">
                        ，还剩
                        {{ cleanupProgress.orphans - cleanupProgress.deleted }} 个未删
                      </span>
                      <span v-if="cleanupProgress.failed">
                        ；失败 {{ cleanupProgress.failed }} 个</span
                      >
                      <span v-if="cleanupProgress.cancelled">，已手动停止</span>
                    </p>
                    <ul v-if="cleanupProgress.sample.length" class="orphan-list">
                      <li v-for="(f, i) in cleanupProgress.sample" :key="i">{{ f }}</li>
                    </ul>
                    <p
                      v-if="cleanupProgress.orphans > cleanupProgress.sample.length"
                      class="set-hint"
                    >
                      仅列出前 {{ cleanupProgress.sample.length }} 个。
                    </p>

                    <div v-if="cleanupReadyToDelete" class="migrate-actions">
                      <button
                        v-if="!cleanupConfirming"
                        class="danger"
                        @click="cleanupConfirming = true"
                      >
                        删除这 {{ cleanupProgress.orphans }} 个孤儿文件
                      </button>
                      <template v-else>
                        <span class="set-hint danger-text">删除不可恢复，确定？</span>
                        <button class="danger" :disabled="cleanupBusy" @click="runCleanup">
                          确认删除
                        </button>
                        <button class="quiet" @click="cleanupConfirming = false">取消</button>
                      </template>
                    </div>
                    <p
                      v-else-if="cleanupProgress.phase === 'scan' && cleanupProgress.orphans === 0"
                      class="set-hint"
                    >
                      没有可清理的孤儿文件。
                    </p>
                    <ul v-if="cleanupProgress.errors.length" class="migrate-errors">
                      <li v-for="(err, i) in cleanupProgress.errors" :key="i">{{ err }}</li>
                    </ul>
                  </template>
                </template>
                <p v-if="cleanupStatus" class="set-status">{{ cleanupStatus }}</p>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  </section>
</template>
