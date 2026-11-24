<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { listImages } from '../api'
import type { ImageInfo } from '../types'
import Lightbox from './Lightbox.vue'

const PAGE = 60

const items = ref<ImageInfo[]>([])
const total = ref(0)
const loading = ref(true)
const loadingMore = ref(false)
const error = ref('')
const selected = ref<ImageInfo | null>(null)

async function refresh() {
  error.value = ''
  loading.value = true
  try {
    const r = await listImages(PAGE, null)
    total.value = r.total
    items.value = r.items
  } catch (e) {
    error.value = (e as Error).message
  } finally {
    loading.value = false
  }
}

async function loadMore() {
  const last = items.value[items.value.length - 1]
  if (!last) return
  loadingMore.value = true
  try {
    const r = await listImages(PAGE, last.id)
    total.value = r.total
    items.value.push(...r.items)
  } catch (e) {
    error.value = (e as Error).message
  } finally {
    loadingMore.value = false
  }
}

function onDeleted(img: ImageInfo) {
  items.value = items.value.filter((i) => i.id !== img.id)
  total.value = Math.max(0, total.value - 1)
  selected.value = null
}

onMounted(refresh)
</script>

<template>
  <section class="gallery" aria-label="图库">
    <div class="gallery-head">
      <h2>图库</h2>
      <span class="count">{{ loading ? '…' : `共 ${total} 张` }}</span>
    </div>

    <p v-if="error" class="gallery-error">{{ error }}</p>
    <p v-else-if="!loading && total === 0" class="empty">
      图库还是空的，把一张图片拖到上面试试。
    </p>

    <div v-else class="grid">
      <button
        v-for="img in items"
        :key="img.id"
        class="cell"
        @click="selected = img"
      >
        <img :src="img.thumb_url" :alt="img.orig_name" loading="lazy" />
        <span class="cell-name">{{ img.orig_name }}</span>
      </button>
    </div>

    <div v-if="items.length < total" class="more-row">
      <button :disabled="loadingMore" @click="loadMore">
        {{ loadingMore ? '正在加载' : `加载更多（还剩 ${total - items.length} 张）` }}
      </button>
    </div>
  </section>

  <Lightbox
    v-if="selected"
    :image="selected"
    @close="selected = null"
    @deleted="onDeleted"
  />
</template>
