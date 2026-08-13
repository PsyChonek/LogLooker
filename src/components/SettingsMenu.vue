<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core';
import { onBeforeUnmount, onMounted, ref } from 'vue';

const open = ref(false);
const busy = ref(false);
const error = ref<string | null>(null);
const root = ref<HTMLElement | null>(null);

function toggle() {
  open.value = !open.value;
  if (open.value) error.value = null;
}

function onDocumentClick(event: MouseEvent) {
  if (root.value && !root.value.contains(event.target as Node)) {
    open.value = false;
  }
}

async function openSettingsFolder() {
  busy.value = true;
  error.value = null;
  try {
    await invoke('open_config_dir');
    open.value = false;
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

onMounted(() => document.addEventListener('click', onDocumentClick));
onBeforeUnmount(() => document.removeEventListener('click', onDocumentClick));
</script>

<template>
  <div ref="root" class="relative">
    <button
      class="p-1.5 rounded transition-colors text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 hover:bg-gray-100 dark:hover:bg-gray-700"
      title="Settings"
      aria-label="Settings"
      aria-haspopup="menu"
      :aria-expanded="open"
      @click="toggle"
    >
      <svg class="w-4 h-4" fill="currentColor" viewBox="0 0 20 20" aria-hidden="true">
        <path
          fill-rule="evenodd"
          d="M11.49 3.17c-.38-1.56-2.6-1.56-2.98 0a1.53 1.53 0 01-2.29.95c-1.37-.84-2.94.73-2.1 2.1a1.53 1.53 0 01-.95 2.29c-1.56.38-1.56 2.6 0 2.98a1.53 1.53 0 01.95 2.29c-.84 1.37.73 2.94 2.1 2.1a1.53 1.53 0 012.29.95c.38 1.56 2.6 1.56 2.98 0a1.53 1.53 0 012.29-.95c1.37.84 2.94-.73 2.1-2.1a1.53 1.53 0 01.95-2.29c1.56-.38 1.56-2.6 0-2.98a1.53 1.53 0 01-.95-2.29c.84-1.37-.73-2.94-2.1-2.1a1.53 1.53 0 01-2.29-.95zM10 13a3 3 0 100-6 3 3 0 000 6z"
          clip-rule="evenodd"
        />
      </svg>
    </button>

    <div
      v-if="open"
      class="absolute right-0 top-full mt-2 z-50 w-60 rounded-lg border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800 shadow-lg p-2"
      role="menu"
    >
      <button
        class="w-full flex items-center gap-2 px-2 py-1.5 rounded text-sm text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors disabled:opacity-50"
        :disabled="busy"
        role="menuitem"
        @click="openSettingsFolder"
      >
        <svg class="w-4 h-4 shrink-0" fill="currentColor" viewBox="0 0 20 20" aria-hidden="true">
          <path
            d="M2 5a2 2 0 012-2h3.59a2 2 0 011.41.59L10.41 5H16a2 2 0 012 2v7a2 2 0 01-2 2H4a2 2 0 01-2-2V5z"
          />
        </svg>
        <span>{{ busy ? 'Opening...' : 'Open settings folder' }}</span>
      </button>
      <p v-if="error" class="px-2 pt-2 text-xs text-red-600 dark:text-red-400" role="alert">
        {{ error }}
      </p>
    </div>
  </div>
</template>
