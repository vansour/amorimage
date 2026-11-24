<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { copyText, formatsFor, uploadFiles } from '../api'
import type { UploadResult } from '../types'

const props = defineProps<{ maxFileMb: number | null }>()

const fileInput = ref<HTMLInputElement | null>(null)
const dragging = ref(false)
const uploading = ref(false)
const progress = ref(0)
const results = ref<UploadResult[]>([])
const copiedKey = ref('')
let copyTimer: number | undefined

function pick() {
  if (!uploading.value) fileInput.value?.click()
}

function onChange(e: Event) {
  const input = e.target as HTMLInputElement
  void doUpload(Array.from(input.files ?? []))
  input.value = ''
}

function onDrop(e: DragEvent) {
  dragging.value = false
  void doUpload(Array.from(e.dataTransfer?.files ?? []))
}

async function doUpload(files: File[]) {
  if (!files.length || uploading.value) return
  uploading.value = true
  progress.value = 0
  try {
    const rs = await uploadFiles(files, (p) => {
      progress.value = p
    })
    results.value = rs
  } catch (e) {
    results.value = [
      {
        name: files.map((f) => f.name).join('、') || '上传',
        status: 'error',
        message: (e as Error).message,
      },
    ]
  } finally {
    uploading.value = false
  }
}

function onPaste(e: ClipboardEvent) {
  const target = e.target as HTMLElement | null
  if (target && /^(INPUT|TEXTAREA)$/.test(target.tagName)) return
  const files = Array.from(e.clipboardData?.files ?? [])
  if (files.length) {
    e.preventDefault()
    void doUpload(files)
  }
}

function copy(key: string, text: string) {
  void copyText(text).then((ok) => {
    if (!ok) return
    copiedKey.value = key
    window.clearTimeout(copyTimer)
    copyTimer = window.setTimeout(() => {
      copiedKey.value = ''
    }, 1600)
  })
}

onMounted(() => window.addEventListener('paste', onPaste))
onBeforeUnmount(() => window.removeEventListener('paste', onPaste))
</script>

<template>
  <section class="upload" aria-label="上传">
    <div
      class="dropzone"
      :class="{ active: dragging, busy: uploading }"
      role="button"
      tabindex="0"
      aria-label="拖拽、粘贴或点击上传图片"
      @click="pick"
      @keydown.enter="pick"
      @keydown.space.prevent="pick"
      @dragover.prevent="dragging = true"
      @dragleave="dragging = false"
      @drop.prevent="onDrop"
    >
      <input
        ref="fileInput"
        type="file"
        multiple
        accept="image/png,image/jpeg,image/gif,image/webp"
        hidden
        @change="onChange"
      />
      <p class="drop-main">
        {{ uploading ? `正在上传 ${Math.round(progress * 100)}%` : '把图片拖进来' }}
      </p>
      <p class="drop-sub">
        {{
          uploading
            ? '松口气，马上就好'
            : `或点击选择文件 · Ctrl+V 直接粘贴 · 单张${
                props.maxFileMb === null ? '无大小限制' : ` ≤ ${props.maxFileMb}MB`
              }`
        }}
      </p>
    </div>

    <div v-if="results.length" class="upload-results" aria-live="polite">
      <div
        v-for="(r, i) in results"
        :key="`${i}-${r.name}`"
        class="upload-row"
        :class="r.status"
      >
        <span class="row-glyph" aria-hidden="true">
          {{ r.status === 'ok' ? '✓' : r.status === 'duplicate' ? '⟳' : '✕' }}
        </span>
        <span class="row-name">{{ r.name }}</span>
        <span v-if="r.message" class="row-msg">{{ r.message }}</span>
        <span v-if="r.image" class="row-actions">
          <button
            v-for="f in formatsFor(r.image)"
            :key="f.label"
            class="mini"
            @click="copy(`${r.name}:${f.label}`, f.text)"
          >
            {{ copiedKey === `${r.name}:${f.label}` ? '已复制' : f.label }}
          </button>
        </span>
      </div>
    </div>
  </section>
</template>
