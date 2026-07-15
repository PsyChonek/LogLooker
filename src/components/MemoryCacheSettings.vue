<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { formatBytes, useAppStore } from '@/stores/appStore';

// The backend is the source of truth: it clamps the cap and reports what it
// actually holds. Reading the state from it (rather than from the config the
// main window loaded) keeps this working in the chart and raw-file windows too.
const store = useAppStore();

const PRESETS = [
  { mb: 512, label: '512M' },
  { mb: 1024, label: '1G' },
  { mb: 2048, label: '2G' },
  { mb: 4096, label: '4G' },
  { mb: 8192, label: '8G' },
];

const open = ref(false);
const root = ref<HTMLElement | null>(null);
const busy = ref(false);
const error = ref<string | null>(null);

const stats = computed(() => store.memoryCache);
const enabled = computed(() => stats.value?.enabled ?? false);
const maxMb = computed(() => (stats.value ? Math.round(stats.value.maxBytes / 1048576) : 2048));

const usedPercent = computed(() =>
  stats.value?.maxBytes ? (stats.value.usedBytes / stats.value.maxBytes) * 100 : 0,
);

async function run(action: () => Promise<void>) {
  busy.value = true;
  error.value = null;
  try {
    await action();
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

function apply(next: { enabled?: boolean; maxMb?: number }) {
  return run(() => store.setMemoryCache(next.enabled ?? enabled.value, next.maxMb ?? maxMb.value));
}

function refresh() {
  return run(() => store.loadMemoryCache());
}

function toggle() {
  open.value = !open.value;
  if (open.value) refresh();
}

function onDocumentClick(event: MouseEvent) {
  if (root.value && !root.value.contains(event.target as Node)) {
    open.value = false;
  }
}

onMounted(() => {
  document.addEventListener('click', onDocumentClick);
  // Pull the live state so the icon reflects an enabled cache right away,
  // rather than staying gray until the panel is first opened.
  refresh();
});
onBeforeUnmount(() => document.removeEventListener('click', onDocumentClick));
</script>

<template>
  <div
    ref="root"
    class="relative"
  >
    <button
      class="p-1.5 rounded transition-colors hover:bg-gray-100 dark:hover:bg-gray-700"
      :class="
        enabled
          ? 'text-blue-600 dark:text-blue-400'
          : 'text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200'
      "
      :title="enabled ? 'Memory cache: on' : 'Memory cache: off'"
      @click="toggle"
    >
      <svg
        class="w-4 h-4"
        fill="currentColor"
        viewBox="0 0 20 20"
      >
        <path
          fill-rule="evenodd"
          d="M7 2a1 1 0 011 1v1h1V3a1 1 0 112 0v1h1V3a1 1 0 112 0v1a2 2 0 012 2h1a1 1 0 110 2h-1v1h1a1 1 0 110 2h-1v1h1a1 1 0 110 2h-1a2 2 0 01-2 2v1a1 1 0 11-2 0v-1H9v1a1 1 0 11-2 0v-1H6a2 2 0 01-2-2H3a1 1 0 110-2h1v-1H3a1 1 0 110-2h1V8H3a1 1 0 010-2h1a2 2 0 012-2V3a1 1 0 011-1zm-1 4v8h8V6H6zm2 2h4v4H8V8z"
          clip-rule="evenodd"
        />
      </svg>
    </button>

    <div
      v-if="open"
      class="absolute right-0 top-full mt-2 z-50 w-64 rounded-lg border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800 shadow-lg p-3 space-y-3"
    >
      <div class="flex items-center justify-between">
        <span class="text-sm font-medium text-gray-800 dark:text-gray-100">Memory cache</span>
        <div class="flex rounded-md bg-gray-100 dark:bg-gray-700 p-0.5 text-xs">
          <button
            v-for="option in [
              { value: false, label: 'Off' },
              { value: true, label: 'On' },
            ]"
            :key="option.label"
            :disabled="busy"
            :class="[
              'px-2.5 py-1 rounded transition-colors disabled:opacity-50',
              enabled === option.value
                ? 'bg-white dark:bg-gray-800 text-gray-800 dark:text-gray-100 font-medium shadow-sm'
                : 'text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200',
            ]"
            @click="apply({ enabled: option.value })"
          >
            {{ option.label }}
          </button>
        </div>
      </div>

      <p class="text-xs text-gray-500 dark:text-gray-400">
        Keeps log text in RAM, where a search can run on every core and a raw file opens without
        unpacking. Logs too big for the limit are read from disk as before.
      </p>

      <div :class="enabled ? '' : 'opacity-40 pointer-events-none'">
        <div class="text-xs text-gray-500 dark:text-gray-400 mb-1">
          Max RAM
        </div>
        <div class="flex rounded-md bg-gray-100 dark:bg-gray-700 p-0.5 text-xs">
          <button
            v-for="preset in PRESETS"
            :key="preset.mb"
            :disabled="busy"
            :class="[
              'flex-1 py-1 rounded transition-colors disabled:opacity-50',
              maxMb === preset.mb
                ? 'bg-white dark:bg-gray-800 text-gray-800 dark:text-gray-100 font-medium shadow-sm'
                : 'text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200',
            ]"
            @click="apply({ maxMb: preset.mb })"
          >
            {{ preset.label }}
          </button>
        </div>
      </div>

      <div
        v-if="stats && enabled"
        class="space-y-1.5"
      >
        <div class="flex h-1.5 rounded-full bg-gray-100 dark:bg-gray-700 overflow-hidden">
          <div
            class="bg-blue-500 h-full"
            :style="{ width: `${usedPercent}%` }"
          />
        </div>
        <div class="flex items-center justify-between text-xs text-gray-500 dark:text-gray-400">
          <span>{{ formatBytes(stats.usedBytes) }} of {{ formatBytes(stats.maxBytes) }}</span>
          <span>{{ stats.files }} {{ stats.files === 1 ? 'file' : 'files' }}</span>
        </div>
      </div>

      <p
        v-if="error"
        class="text-xs text-red-600 dark:text-red-400"
      >
        {{ error }}
      </p>

      <div class="flex gap-2">
        <button
          class="flex-1 py-1 rounded text-xs border border-gray-200 dark:border-gray-600 text-gray-600 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors disabled:opacity-50"
          :disabled="busy"
          @click="refresh"
        >
          Refresh
        </button>
        <button
          class="flex-1 py-1 rounded text-xs border border-gray-200 dark:border-gray-600 text-gray-600 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors disabled:opacity-50"
          :disabled="busy || !stats?.files"
          @click="run(store.clearMemoryCache)"
        >
          Clear cache
        </button>
      </div>
    </div>
  </div>
</template>
