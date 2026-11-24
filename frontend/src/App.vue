<script setup lang="ts">
import { onMounted, ref } from 'vue'
import UploadPanel from './components/UploadPanel.vue'
import Gallery from './components/Gallery.vue'
import SettingsView from './components/SettingsView.vue'
import { getSettings } from './api'
import type { SiteSettings } from './types'

type View = 'upload' | 'gallery' | 'settings'

const nav: { key: View; label: string }[] = [
  { key: 'upload', label: '上传' },
  { key: 'gallery', label: '图库' },
  { key: 'settings', label: '设置' },
]

const view = ref<View>('upload')
const siteTitle = ref('amorimage')
const maxFileMb = ref<number | null>(null)

function applySettings(s: SiteSettings) {
  siteTitle.value = s.title
  maxFileMb.value = s.max_file_mb
  document.title = s.title
  document.documentElement.dataset.theme = s.theme
}

onMounted(async () => {
  try {
    applySettings(await getSettings())
  } catch {
    // 读取失败时保持默认标题与暗色主题
  }
})

async function logout() {
  try {
    await fetch('/api/logout', { method: 'POST' })
  } catch {
    // 网络异常也同样回到登录页
  }
  location.replace('/')
}
</script>

<template>
  <div class="container">
    <header class="app-head">
      <div class="head-left">
        <h1 class="brand"><span class="brand-mark" aria-hidden="true"></span>{{ siteTitle }}</h1>
        <nav class="app-nav">
          <button
            v-for="n in nav"
            :key="n.key"
            :class="{ on: view === n.key }"
            @click="view = n.key"
          >
            {{ n.label }}
          </button>
        </nav>
      </div>
      <button class="quiet" @click="logout">登出</button>
    </header>

    <UploadPanel v-if="view === 'upload'" :max-file-mb="maxFileMb" />
    <Gallery v-else-if="view === 'gallery'" />
    <SettingsView v-else @changed="applySettings" />
  </div>
</template>
