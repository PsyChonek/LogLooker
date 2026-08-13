<script setup lang="ts">
import MemoryCacheSettings from '@/components/MemoryCacheSettings.vue';
import SettingsMenu from '@/components/SettingsMenu.vue';
import ThemeSelector from '@/components/ThemeSelector.vue';
import TimeModeSelector from '@/components/TimeModeSelector.vue';

defineProps<{
  title: string;
  error?: string | null;
  sticky?: boolean;
}>();

const emit = defineEmits<{
  'dismiss-error': [];
}>();
</script>

<template>
  <header
    :class="[
      'bg-white dark:bg-gray-800 border-b border-gray-200 dark:border-gray-700 px-6 py-3',
      sticky ? 'sticky top-0 z-50' : '',
    ]"
  >
    <div class="flex items-center gap-6">
      <span class="text-lg font-semibold text-gray-800 dark:text-gray-100">{{ title }}</span>

      <slot name="nav" />

      <div class="ml-auto flex items-center gap-3">
        <slot name="controls" />

        <TimeModeSelector />
        <MemoryCacheSettings />
        <ThemeSelector />
        <SettingsMenu />
      </div>
    </div>
  </header>

  <div
    v-if="error"
    class="bg-red-50 dark:bg-red-900/30 border-b border-red-200 dark:border-red-800 px-6 py-2 flex items-center justify-between"
  >
    <span class="text-sm text-red-700 dark:text-red-400">{{ error }}</span>
    <button
      class="text-red-400 hover:text-red-600 dark:text-red-500 dark:hover:text-red-300 text-sm"
      @click="emit('dismiss-error')"
    >
      Dismiss
    </button>
  </div>
</template>
