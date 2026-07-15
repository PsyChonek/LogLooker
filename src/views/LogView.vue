<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue';
import { useLog } from '@/composables/useLog';

const { logs, refreshLogs, clearLogs } = useLog();
const autoRefresh = ref(false);
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
      <div class="max-h-[calc(100vh-180px)] overflow-y-auto">
        <table class="w-full text-xs font-mono">
          <thead class="sticky top-0 bg-white dark:bg-gray-800">
            <tr class="border-b border-gray-200 dark:border-gray-700">
              <th class="text-left py-2 px-3 font-medium text-gray-500 dark:text-gray-400 w-44">
                Time
              </th>
              <th class="text-left py-2 px-3 font-medium text-gray-500 dark:text-gray-400 w-16">
                Level
              </th>
              <th class="text-left py-2 px-3 font-medium text-gray-500 dark:text-gray-400 w-24">
                Source
              </th>
              <th class="text-left py-2 px-3 font-medium text-gray-500 dark:text-gray-400">
                Message
              </th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="(entry, i) in [...logs].reverse()"
              :key="i"
              class="border-b border-gray-100 dark:border-gray-800"
              :class="{
                'bg-red-50/50 dark:bg-red-900/10': entry.level === 'error',
                'bg-yellow-50/50 dark:bg-yellow-900/10': entry.level === 'warn',
              }"
            >
              <td class="py-1.5 px-3 text-gray-500 dark:text-gray-400 whitespace-nowrap">
                {{ entry.timestamp }}
              </td>
              <td class="py-1.5 px-3">
                <span
                  class="inline-block px-1.5 py-0.5 text-[10px] font-semibold rounded uppercase"
                  :class="{
                    'bg-blue-100 dark:bg-blue-900/30 text-blue-700 dark:text-blue-400':
                      entry.level === 'info',
                    'bg-yellow-200 dark:bg-yellow-800 text-yellow-800 dark:text-yellow-200':
                      entry.level === 'warn',
                    'bg-red-100 dark:bg-red-900/30 text-red-600 dark:text-red-400':
                      entry.level === 'error',
                  }"
                >
                  {{ entry.level }}
                </span>
              </td>
              <td class="py-1.5 px-3 text-gray-500 dark:text-gray-400">
                {{ entry.source }}
              </td>
              <td class="py-1.5 px-3 text-gray-700 dark:text-gray-300 break-all">
                {{ entry.message }}
              </td>
            </tr>
            <tr v-if="logs.length === 0">
              <td
                colspan="4"
                class="py-8 text-center text-gray-400"
              >
                No log entries yet.
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>
  </div>
</template>
