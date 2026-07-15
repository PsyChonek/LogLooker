<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue';
import { useTimeMode } from '@/composables/useTimeMode';

const { mode } = useTimeMode();

const open = ref(false);
const root = ref<HTMLElement | null>(null);

function onDocumentClick(event: MouseEvent) {
  if (root.value && !root.value.contains(event.target as Node)) {
    open.value = false;
  }
}

onMounted(() => document.addEventListener('click', onDocumentClick));
onBeforeUnmount(() => document.removeEventListener('click', onDocumentClick));
</script>

<template>
  <div
    ref="root"
    class="relative"
  >
    <button
      class="p-1.5 rounded transition-colors text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 hover:bg-gray-100 dark:hover:bg-gray-700"
      :title="`Time zone: ${mode === 'utc' ? 'UTC' : 'Local'}`"
      @click="open = !open"
    >
      <svg
        class="w-4 h-4"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        viewBox="0 0 24 24"
      >
        <circle
          cx="12"
          cy="12"
          r="9"
        />
        <path
          stroke-linecap="round"
          stroke-linejoin="round"
          d="M12 7v5l3 2"
        />
      </svg>
    </button>

    <div
      v-if="open"
      class="absolute right-0 top-full mt-2 z-50 w-56 rounded-lg border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800 shadow-lg p-3 space-y-2"
    >
      <span class="text-sm font-medium text-gray-800 dark:text-gray-100">Time zone</span>

      <div class="flex rounded-md bg-gray-100 dark:bg-gray-700 p-0.5 text-xs">
        <button
          v-for="option in (['utc', 'local'] as const)"
          :key="option"
          :class="[
            'flex-1 py-1 rounded transition-colors',
            mode === option
              ? 'bg-white dark:bg-gray-800 text-gray-800 dark:text-gray-100 font-medium shadow-sm'
              : 'text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200',
          ]"
          @click="mode = option"
        >
          {{ option === 'utc' ? 'UTC' : 'Local' }}
        </button>
      </div>

      <p class="text-xs text-gray-500 dark:text-gray-400">
        The clock log times are read on. Each service's own clock is detected from the timestamps of
        the files it writes.
      </p>
    </div>
  </div>
</template>
