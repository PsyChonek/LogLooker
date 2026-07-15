<script setup lang="ts">
import { ref, onMounted, onBeforeUnmount } from 'vue';
import { useTheme } from '@/composables/useTheme';

const { theme, dark, themes } = useTheme();

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
      title="Theme"
      @click="open = !open"
    >
      <svg
        class="w-4 h-4"
        fill="currentColor"
        viewBox="0 0 20 20"
      >
        <path
          fill-rule="evenodd"
          d="M4 2a2 2 0 00-2 2v11a3 3 0 106 0V4a2 2 0 00-2-2H4zm1 14a1 1 0 100-2 1 1 0 000 2zm5-1.757l4.9-4.9a2 2 0 000-2.828L13.485 5.1a2 2 0 00-2.828 0L10 5.757v8.486zM16 18H9.071l6-6H16a2 2 0 012 2v2a2 2 0 01-2 2z"
          clip-rule="evenodd"
        />
      </svg>
    </button>

    <div
      v-if="open"
      class="absolute right-0 top-full mt-2 z-50 w-52 rounded-lg border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800 shadow-lg p-2"
    >
      <div class="flex rounded-md bg-gray-100 dark:bg-gray-700 p-0.5 mb-2 text-xs">
        <button
          v-for="variant in [
            { isDark: false, label: 'Light' },
            { isDark: true, label: 'Dark' },
          ]"
          :key="variant.label"
          :class="[
            'flex-1 py-1 rounded transition-colors',
            dark === variant.isDark
              ? 'bg-white dark:bg-gray-800 text-gray-800 dark:text-gray-100 font-medium shadow-sm'
              : 'text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200',
          ]"
          @click="dark = variant.isDark"
        >
          {{ variant.label }}
        </button>
      </div>

      <button
        v-for="t in themes"
        :key="t.id"
        :class="[
          'w-full flex items-center gap-2 px-2 py-1.5 rounded text-sm transition-colors',
          theme === t.id
            ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-700 dark:text-blue-300'
            : 'text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700',
        ]"
        @click="theme = t.id"
      >
        <span class="flex -space-x-1">
          <span
            class="w-3.5 h-3.5 rounded-full border border-black/10 dark:border-white/20"
            :style="{ backgroundColor: t.preview.accent }"
          />
          <span
            class="w-3.5 h-3.5 rounded-full border border-black/10 dark:border-white/20"
            :style="{ backgroundColor: dark ? t.preview.surfaceDark : t.preview.surfaceLight }"
          />
        </span>
        <span>{{ t.name }}</span>
        <svg
          v-if="theme === t.id"
          class="w-3.5 h-3.5 ml-auto"
          fill="currentColor"
          viewBox="0 0 20 20"
        >
          <path
            fill-rule="evenodd"
            d="M16.707 5.293a1 1 0 010 1.414l-8 8a1 1 0 01-1.414 0l-4-4a1 1 0 011.414-1.414L8 12.586l7.293-7.293a1 1 0 011.414 0z"
            clip-rule="evenodd"
          />
        </svg>
      </button>
    </div>
  </div>
</template>
