<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue';
import DataTable, { type DataTableColumn } from '@/components/DataTable.vue';
import { useLog } from '@/composables/useLog';

const { logs, refreshLogs, clearLogs } = useLog();
const autoRefresh = ref(false);

const columns: DataTableColumn[] = [
  { key: 'time', label: 'Time', width: 190, fitContent: true, numeric: true },
  { key: 'level', label: 'Level', width: 72 },
  { key: 'source', label: 'Source', width: 120 },
  { key: 'message', label: 'Message' },
];

// Newest first; the reverse is a copy so the source array keeps its order
const rows = computed(() => [...logs.value].reverse());
let interval: ReturnType<typeof setInterval> | null = null;

function toggleAutoRefresh() {
  autoRefresh.value = !autoRefresh.value;
  if (autoRefresh.value) {
    interval = setInterval(() => refreshLogs(), 2000);
  } else if (interval) {
    clearInterval(interval);
    interval = null;
  }
}

onMounted(() => {
  refreshLogs();
});

onUnmounted(() => {
  if (interval) {
    clearInterval(interval);
    interval = null;
  }
});
</script>

<template>
  <div>
    <div class="flex items-center justify-between mb-4">
      <h1 class="text-lg font-semibold text-gray-800 dark:text-gray-100">
        Log
      </h1>
      <div class="flex items-center gap-2">
        <button
          class="px-3 py-1.5 text-xs font-medium rounded-md transition-colors"
          :class="
            autoRefresh
              ? 'bg-blue-500 text-white'
              : 'border border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200'
          "
          @click="toggleAutoRefresh"
        >
          {{ autoRefresh ? 'Auto-refresh ON' : 'Auto-refresh' }}
        </button>
        <button
          class="px-3 py-1.5 text-xs font-medium border border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 rounded-md transition-colors"
          @click="refreshLogs()"
        >
          Refresh
        </button>
        <button
          class="px-3 py-1.5 text-xs font-medium border border-gray-300 dark:border-gray-600 text-red-500 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-900/20 rounded-md transition-colors"
          @click="clearLogs()"
        >
          Clear
        </button>
      </div>
    </div>

    <div
      class="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden"
    >
      <DataTable
        table-id="log"
        :columns="columns"
        :rows="rows"
        table-class="font-mono"
        scroll-class="max-h-[calc(100vh-180px)]"
        :row-class="(entry) => ({
          'bg-red-50/50 dark:bg-red-900/10': entry.level === 'error',
          'bg-yellow-50/50 dark:bg-yellow-900/10': entry.level === 'warn',
        })"
      >
        <template #time="{ row }">
          <span class="text-gray-500 dark:text-gray-400 whitespace-nowrap">{{ row.timestamp }}</span>
        </template>
        <template #level="{ row }">
          <span
            class="inline-block px-1.5 py-0.5 text-[10px] font-semibold rounded uppercase"
            :class="{
              'bg-blue-100 dark:bg-blue-900/30 text-blue-700 dark:text-blue-400':
                row.level === 'info',
              'bg-yellow-200 dark:bg-yellow-800 text-yellow-800 dark:text-yellow-200':
                row.level === 'warn',
              'bg-red-100 dark:bg-red-900/30 text-red-600 dark:text-red-400':
                row.level === 'error',
            }"
          >
            {{ row.level }}
          </span>
        </template>
        <template #source="{ row }">
          <span class="text-gray-500 dark:text-gray-400">{{ row.source }}</span>
        </template>
        <template #message="{ row }">
          <span class="text-gray-700 dark:text-gray-300 break-all">{{ row.message }}</span>
        </template>
        <template #empty>
          No log entries yet.
        </template>
      </DataTable>
    </div>
  </div>
</template>
