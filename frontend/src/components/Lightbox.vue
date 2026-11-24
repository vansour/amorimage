<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { copyText, deleteImage, formatsFor } from '../api'
import type { ImageInfo } from '../types'
import { fmtBytes, fmtDate } from '../utils'

const props = defineProps<{ image: ImageInfo }>()
const emit = defineEmits<{ close: []; deleted: [img: ImageInfo] }>()

const confirming = ref(false)
const deleting = ref(false)
const error = ref('')
const copiedKey = ref('')
let confirmTimer: number | undefined
let copyTimer: number | undefined

const formats = computed(() => formatsFor(props.image))

function onKey(e: KeyboardEvent) {
  if (e.key === 'Escape') emit('close')
}

function copy(label: string, text: string) {
  void copyText(text).then((ok) => {
    if (!ok) return
    copiedKey.value = label
    window.clearTimeout(copyTimer)
    copyTimer = window.setTimeout(() => {
      copiedKey.value = ''
    }, 1600)
  })
}

async function onDelete() {
  if (!confirming.value) {
    confirming.value = true
    confirmTimer = window.setTimeout(() => {
      confirming.value = false
    }, 3000)
    return
  }
  window.clearTimeout(confirmTimer)
  deleting.value = true
  try {
    await deleteImage(props.image.id)
    emit('deleted', props.image)
  } catch (e) {
    error.value = (e as Error).message
    confirming.value = false
  } finally {
    deleting.value = false
  }
}

onMounted(() => {
  window.addEventListener('keydown', onKey)
  document.body.style.overflow = 'hidden'
})

onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKey)
  document.body.style.overflow = ''
})
</script>

<template>
  <Teleport to="body">
    <div class="lightbox" @click.self="emit('close')">
      <figure class="lb-figure">
        <img :src="image.url" :alt="image.orig_name" />
      </figure>
      <div class="lb-panel">
        <p class="lb-name">{{ image.orig_name }}</p>
        <dl class="lb-meta">
          <div><dt>尺寸</dt><dd>{{ image.width }} × {{ image.height }}</dd></div>
          <div><dt>大小</dt><dd>{{ fmtBytes(image.size) }}</dd></div>
          <div><dt>时间</dt><dd>{{ fmtDate(image.created_at) }}</dd></div>
        </dl>
        <div class="lb-copy">
          <button v-for="f in formats" :key="f.label" @click="copy(f.label, f.text)">
            {{ copiedKey === f.label ? '已复制' : `复制${f.label}` }}
          </button>
        </div>
        <p v-if="error" class="lb-error">{{ error }}</p>
        <div class="lb-foot">
          <button class="danger" :disabled="deleting" @click="onDelete">
            {{ confirming ? '再点一次确认删除' : '删除' }}
          </button>
          <button @click="emit('close')">关闭（Esc）</button>
        </div>
      </div>
    </div>
  </Teleport>
</template>
