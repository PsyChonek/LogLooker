<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { save } from '@tauri-apps/plugin-dialog';
import BaseCheckbox from '@/components/BaseCheckbox.vue';
import ConfirmDialog from '@/components/ConfirmDialog.vue';
import DataTable, { type DataTableColumn, type SortState } from '@/components/DataTable.vue';
import RawFileViewer from '@/components/RawFileViewer.vue';
import { formatBytes, useAppStore } from '@/stores/appStore';
import { sortRows, type SortAccessor } from '@/utils/tableSort';
import type { CachedFileInfo } from '@/types';

const columns: DataTableColumn[] = [
  { key: 'service', label: 'Service', width: 128, sortable: true },
  { key: 'file', label: 'File', sortable: true },
  { key: 'date', label: 'Date', width: 112, sortable: true, numeric: true },
  { key: 'instance', label: 'Instance', width: 160, sortable: true },
  { key: 'size', label: 'Size', width: 112, align: 'right', sortable: true, numeric: true },
  { key: 'cached', label: 'Cached', width: 96, align: 'right', sortable: true, numeric: true },
  { key: 'actions', label: '', width: 232, align: 'right', reorderable: false, hideable: false },
];

const store = useAppStore();
const files = ref<CachedFileInfo[]>([]);
const loading = ref(false);
const error = ref<string | null>(null);
const notice = ref<string | null>(null);
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

const sort = ref<SortState | null>(null);

// Sort on raw values, not the formatted strings shown in the cells
const sortAccessors: Record<string, SortAccessor<CachedFileInfo>> = {
  service: (file) => serviceNames.value[file.serviceId] ?? file.serviceId,
  file: (file) => file.file,
  date: (file) => file.date,
  instance: (file) => file.instance,
  size: (file) => file.sizeBytes,
  cached: (file) => file.compressedBytes,
};

const sortedFiles = computed(() => sortRows(visibleFiles.value, sort.value, sortAccessors));

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

// Decompress the cached .zst and save it as a plain log file where the user picks
async function exportFile(file: CachedFileInfo) {
  let path: string | null;
  try {
    path = await save({
      defaultPath: file.file,
      filters: [{ name: 'Log', extensions: ['log', 'txt'] }],
    });
  } catch (e) {
    error.value = String(e);
    return;
  }
  if (!path) return;
  error.value = null;
  try {
    const bytes = await invoke<number>('export_cached_file', {
      serviceId: file.serviceId,
      file: file.file,
      targetPath: path,
    });
    notice.value = `Exported ${formatBytes(bytes)} to ${path}`;
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
      query: null,
      isRegex: true,
      caseSensitive: false,
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

    <div
      v-if="notice"
      class="mb-4 px-4 py-2 rounded-md bg-green-50 dark:bg-green-900/30 border border-green-200 dark:border-green-800 text-sm text-green-700 dark:text-green-400 flex items-center justify-between"
    >
      <span class="break-all">{{ notice }}</span>
      <button
        class="ml-4 shrink-0 hover:underline"
        @click="notice = null"
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
      <DataTable
        v-model:sort="sort"
        table-id="files"
        :columns="columns"
        :rows="sortedFiles"
        :row-key="(file) => `${file.serviceId}:${file.file}`"
        table-class="min-w-[56rem] font-mono"
        scroll-class="max-h-[calc(100vh-220px)]"
        :loading="loading"
        clickable-rows
        @row-click="(file) => (opened = file)"
      >
        <template #service="{ row }">
          <span
            class="inline-block px-1.5 py-0.5 text-[10px] font-semibold rounded"
            :class="
              row.serviceId.startsWith('production/')
                ? 'bg-red-100 dark:bg-red-900/30 text-red-700 dark:text-red-400'
                : 'bg-blue-100 dark:bg-blue-900/30 text-blue-700 dark:text-blue-400'
            "
          >
            {{ serviceNames[row.serviceId] ?? row.serviceId }}
          </span>
        </template>
        <template #file="{ row }">
          <span class="text-gray-700 dark:text-gray-300 break-all">{{ row.file }}</span>
        </template>
        <template #date="{ row }">
          <span class="text-gray-500 dark:text-gray-400 whitespace-nowrap">{{ row.date ?? '-' }}</span>
        </template>
        <template #instance="{ row }">
          <span class="block text-gray-400 truncate">{{ row.instance ?? '-' }}</span>
        </template>
        <template #size="{ row }">
          <span class="text-gray-500 dark:text-gray-400 whitespace-nowrap">{{ formatBytes(row.sizeBytes) }}</span>
        </template>
        <template #cached="{ row }">
          <span class="text-gray-400 whitespace-nowrap">{{ formatBytes(row.compressedBytes) }}</span>
        </template>
        <template #actions="{ row }">
          <span class="whitespace-nowrap">
            <button
              class="px-1.5 py-0.5 text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 hover:underline"
              title="Open the raw file in its own window"
              @click.stop="openInWindow(row)"
            >
              Window
            </button>
            <button
              class="px-1.5 py-0.5 text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 hover:underline"
              title="Decompress and save the file to a location you choose"
              @click.stop="exportFile(row)"
            >
              Export
            </button>
            <button
              class="px-1.5 py-0.5 text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 hover:underline"
              title="Show the cached file in Explorer"
              @click.stop="revealFile(row)"
            >
              Reveal
            </button>
            <button
              class="px-1.5 py-0.5 text-gray-400 hover:text-red-600 dark:hover:text-red-400 hover:underline"
              title="Delete the cached file"
              @click.stop="toDelete = row"
            >
              Delete
            </button>
          </span>
        </template>
        <template #empty>
          No cached files. Select services and a date range on the Services page and sync.
        </template>
      </DataTable>
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
