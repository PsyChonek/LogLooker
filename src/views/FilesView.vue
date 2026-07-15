<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import BaseCheckbox from '@/components/BaseCheckbox.vue';
import ConfirmDialog from '@/components/ConfirmDialog.vue';
import RawFileViewer from '@/components/RawFileViewer.vue';
import { formatBytes, useAppStore } from '@/stores/appStore';
import type { CachedFileInfo } from '@/types';

const store = useAppStore();
const files = ref<CachedFileInfo[]>([]);
const loading = ref(false);
const error = ref<string | null>(null);
const filter = ref('');
const selectedOnly = ref(false);
const opened = ref<CachedFileInfo | null>(null);
const toDelete = ref<CachedFileInfo | null>(null);

const serviceNames = computed(() =>
  Object.fromEntries(store.config.services.map((s) => [s.id, s.name])),
);

// Only services of the current environment are listed; "selected only" narrows
// it further to the selection made on the Services page
const visibleFiles = computed(() => {
  const ids = new Set(
    (selectedOnly.value ? store.selectedServices : store.services).map((s) => s.id),
  );
  const needle = filter.value.toLowerCase();
  return files.value.filter(
    (file) =>
      ids.has(file.serviceId) &&
      (!needle ||
        file.file.toLowerCase().includes(needle) ||
        (serviceNames.value[file.serviceId] ?? '').toLowerCase().includes(needle)),
  );
});

const totalBytes = computed(() =>
  visibleFiles.value.reduce((sum, file) => sum + file.sizeBytes, 0),
);

async function load() {
  loading.value = true;
  error.value = null;
  try {
    if (store.config.services.length === 0) await store.loadConfig();
    files.value = await invoke<CachedFileInfo[]>('list_cached_files', { serviceIds: [] });
  } catch (e) {
    error.value = String(e);
  } finally {
    loading.value = false;
  }
}

async function openCacheDir() {
  try {
    await invoke('open_cache_dir', { serviceId: null });
  } catch (e) {
    error.value = String(e);
  }
}

async function revealFile(file: CachedFileInfo) {
  try {
    await invoke('reveal_cached_file', { serviceId: file.serviceId, file: file.file });
  } catch (e) {
    error.value = String(e);
  }
}

async function openInWindow(file: CachedFileInfo) {
  try {
    await invoke('open_raw_window', {
      serviceId: file.serviceId,
      serviceName: serviceNames.value[file.serviceId] ?? file.serviceId,
      file: file.file,
      line: null,
    });
  } catch (e) {
    error.value = String(e);
  }
}

async function deleteFile() {
  const file = toDelete.value;
  toDelete.value = null;
  if (!file) return;
  try {
    await invoke('delete_cached_file', { serviceId: file.serviceId, file: file.file });
    files.value = files.value.filter(
      (f) => !(f.serviceId === file.serviceId && f.file === file.file),
    );
  } catch (e) {
    error.value = String(e);
  }
}

onMounted(load);
</script>

<template>
  <div>
    <div
      v-if="error"
      class="mb-4 px-4 py-2 rounded-md bg-red-50 dark:bg-red-900/30 border border-red-200 dark:border-red-800 text-sm text-red-700 dark:text-red-400 flex items-center justify-between"
    >
      <span class="break-all">{{ error }}</span>
      <button
        class="ml-4 shrink-0 hover:underline"
        @click="error = null"
      >
        Dismiss
      </button>
    </div>

    <div class="flex items-center justify-between mb-4">
      <h1 class="text-lg font-semibold text-gray-800 dark:text-gray-100">
        Cached files
      </h1>
      <div class="flex items-center gap-2">
        <button
          class="px-3 py-1.5 text-xs font-medium border border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 rounded-md transition-colors"
          title="Open the cache directory in Explorer"
          @click="openCacheDir"
        >
          Open cache folder
        </button>
        <button
          class="px-3 py-1.5 text-xs font-medium border border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 rounded-md transition-colors"
          :disabled="loading"
          @click="load"
        >
          {{ loading ? 'Loading...' : 'Refresh' }}
        </button>
      </div>
    </div>

    <div class="flex items-center gap-4 mb-4 text-xs text-gray-600 dark:text-gray-400">
      <input
        v-model="filter"
        type="text"
        spellcheck="false"
        placeholder="Filter by file or service name..."
        class="w-72 px-3 py-1.5 text-sm font-mono rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800"
      >
      <BaseCheckbox v-model="selectedOnly">
        Selected services only
      </BaseCheckbox>
      <span class="ml-auto">
        {{ visibleFiles.length }} files ({{ store.environment }}), {{ formatBytes(totalBytes) }}
        uncompressed
      </span>
    </div>

    <div
      class="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden"
    >
      <div class="max-h-[calc(100vh-220px)] overflow-auto">
        <table class="w-full min-w-[56rem] text-xs font-mono">
          <thead class="sticky top-0 bg-white dark:bg-gray-800">
            <tr class="border-b border-gray-200 dark:border-gray-700">
              <th class="text-left py-2 px-3 font-medium text-gray-500 dark:text-gray-400 w-32">
                Service
              </th>
              <th class="text-left py-2 px-3 font-medium text-gray-500 dark:text-gray-400">
                File
              </th>
              <th class="text-left py-2 px-3 font-medium text-gray-500 dark:text-gray-400 w-28">
                Date
              </th>
              <th class="text-left py-2 px-3 font-medium text-gray-500 dark:text-gray-400 w-40">
                Instance
              </th>
              <th class="text-right py-2 px-3 font-medium text-gray-500 dark:text-gray-400 w-28">
                Size
              </th>
              <th class="text-right py-2 px-3 font-medium text-gray-500 dark:text-gray-400 w-24">
                Cached
              </th>
              <th class="w-44 py-2 px-3" />
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="file in visibleFiles"
              :key="`${file.serviceId}:${file.file}`"
              class="border-b border-gray-100 dark:border-gray-800 hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer"
              title="Open the raw file"
              @click="opened = file"
            >
              <td class="py-1.5 px-3">
                <span
                  class="inline-block px-1.5 py-0.5 text-[10px] font-semibold rounded"
                  :class="
                    file.serviceId.startsWith('production/')
                      ? 'bg-red-100 dark:bg-red-900/30 text-red-700 dark:text-red-400'
                      : 'bg-blue-100 dark:bg-blue-900/30 text-blue-700 dark:text-blue-400'
                  "
                >
                  {{ serviceNames[file.serviceId] ?? file.serviceId }}
                </span>
              </td>
              <td class="py-1.5 px-3 text-gray-700 dark:text-gray-300 break-all">
                {{ file.file }}
              </td>
              <td class="py-1.5 px-3 text-gray-500 dark:text-gray-400 whitespace-nowrap">
                {{ file.date ?? '—' }}
              </td>
              <td class="py-1.5 px-3 text-gray-400 truncate">
                {{ file.instance ?? '—' }}
              </td>
              <td class="py-1.5 px-3 text-right text-gray-500 dark:text-gray-400 whitespace-nowrap">
                {{ formatBytes(file.sizeBytes) }}
              </td>
              <td class="py-1.5 px-3 text-right text-gray-400 whitespace-nowrap">
                {{ formatBytes(file.compressedBytes) }}
              </td>
              <td class="py-1.5 px-3 text-right whitespace-nowrap">
                <button
                  class="px-1.5 py-0.5 text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 hover:underline"
                  title="Open the raw file in its own window"
                  @click.stop="openInWindow(file)"
                >
                  Window
                </button>
                <button
                  class="px-1.5 py-0.5 text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 hover:underline"
                  title="Show the cached file in Explorer"
                  @click.stop="revealFile(file)"
                >
                  Reveal
                </button>
                <button
                  class="px-1.5 py-0.5 text-gray-400 hover:text-red-600 dark:hover:text-red-400 hover:underline"
                  title="Delete the cached file"
                  @click.stop="toDelete = file"
                >
                  Delete
                </button>
              </td>
            </tr>
            <tr v-if="!loading && visibleFiles.length === 0">
              <td
                colspan="7"
                class="py-8 text-center text-gray-400"
              >
                No cached files. Select services and a date range on the Services page and sync.
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>

    <RawFileViewer
      v-if="opened"
      :key="`${opened.serviceId}:${opened.file}`"
      :service-id="opened.serviceId"
      :service-name="serviceNames[opened.serviceId] ?? opened.serviceId"
      :file="opened.file"
      @close="opened = null"
    />

    <ConfirmDialog
      :visible="toDelete !== null"
      :message="`Delete the cached file ${toDelete?.file ?? ''}? The next sync covering its date will download it again.`"
      @confirm="deleteFile"
      @cancel="toDelete = null"
    />
  </div>
</template>
