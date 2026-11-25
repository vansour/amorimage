<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { deleteFavicon, getSettings, probeStorage, updateSettings, uploadFavicon } from '../api'
import type { SiteSettings, StorageBackend, Theme } from '../types'

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

const storageBackend = ref<StorageBackend>('local')
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

async function load() {
  const s = await getSettings()
  title.value = s.title
  maxFileMb.value = s.max_file_mb
  maxBodyMb.value = s.max_body_mb
  theme.value = s.theme
  hasFavicon.value = s.has_favicon
  storageBackend.value = s.storage.backend
  webdavUrl.value = s.storage.webdav_url
  webdavUsername.value = s.storage.webdav_username
  webdavHasPassword.value = s.storage.webdav_has_password
  webdavPassword.value = ''
  s3Endpoint.value = s.storage.s3_endpoint
  s3Bucket.value = s.storage.s3_bucket
  s3Region.value = s.storage.s3_region
  s3AccessKey.value = s.storage.s3_access_key
  s3HasSecret.value = s.storage.s3_has_secret
  s3SecretKey.value = ''
  s3PathStyle.value = s.storage.s3_path_style
  emit('changed', s)
}

/** 只带上当前后端的字段；密码/密钥留空即保留原值 */
function storagePatch() {
  const patch: Parameters<typeof updateSettings>[0]['storage'] = {
    backend: storageBackend.value,
  }
  if (storageBackend.value === 'webdav') {
    patch.webdav_url = webdavUrl.value.trim()
    patch.webdav_username = webdavUsername.value.trim()
    if (webdavPassword.value) patch.webdav_password = webdavPassword.value
  } else if (storageBackend.value === 's3') {
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
    await updateSettings({ storage: storagePatch() })
    await load()
    storageStatus.value = '已保存'
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
    const r = await probeStorage(storagePatch())
    storageStatus.value = r.message
  } catch (e) {
    storageStatus.value = (e as Error).message
  } finally {
    storageProbing.value = false
  }
}

function passwordHint(has: boolean): string {
  return has ? '留空则保持现有密码' : ''
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

onMounted(load)
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
            <h3>存储后端</h3>
            <p>
              图片与缩略图存放的位置，保存后新上传立即使用；已存图片仍可从原位置读取。
            </p>
          </header>
          <div class="panel-body">
            <div class="form-row">
              <label class="row-label">存放位置</label>
              <div class="segmented">
                <button
                  :class="{ on: storageBackend === 'local' }"
                  @click="storageBackend = 'local'"
                >
                  本地磁盘
                </button>
                <button
                  :class="{ on: storageBackend === 'webdav' }"
                  @click="storageBackend = 'webdav'"
                >
                  WebDAV
                </button>
                <button
                  :class="{ on: storageBackend === 's3' }"
                  @click="storageBackend = 's3'"
                >
                  S3
                </button>
              </div>
            </div>

            <template v-if="storageBackend === 'webdav'">
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

            <template v-else-if="storageBackend === 's3'">
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
              <span v-if="storageStatus" class="set-status">{{ storageStatus }}</span>
            </div>
          </div>
        </div>
      </div>
    </div>
  </section>
</template>
