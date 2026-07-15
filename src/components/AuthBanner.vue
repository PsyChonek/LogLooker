<script setup lang="ts">
import { ref } from 'vue';
import { openUrl } from '@tauri-apps/plugin-opener';
import type { AuthNotice } from '@/utils/authError';

defineProps<{ notice: AuthNotice }>();
const emit = defineEmits<{ dismiss: [] }>();

// Which command was last copied, so we can flash "Copied" on that row only
const copied = ref<string | null>(null);

async function copy(command: string) {
  await navigator.clipboard.writeText(command);
  copied.value = command;
  setTimeout(() => {
    if (copied.value === command) copied.value = null;
  }, 1500);
}
</script>

<template>
  <div
    class="mb-4 rounded-lg border border-amber-300 dark:border-amber-700/60 bg-amber-50 dark:bg-amber-900/20 px-4 py-3"
  >
    <div class="flex items-start gap-3">
      <svg
        class="size-5 shrink-0 mt-0.5 text-amber-500 dark:text-amber-400"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="1.8"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path d="M12 9v4M12 17h.01" />
        <path d="M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0Z" />
      </svg>

      <div class="min-w-0 flex-1">
        <div class="flex items-start justify-between gap-4">
          <h3 class="text-sm font-semibold text-amber-800 dark:text-amber-300">
            {{ notice.title }}
          </h3>
          <button
            class="shrink-0 text-xs text-amber-700/70 dark:text-amber-400/70 hover:text-amber-900 dark:hover:text-amber-200 hover:underline"
            @click="emit('dismiss')"
          >
            Dismiss
          </button>
        </div>

        <p class="mt-1 text-xs text-amber-700 dark:text-amber-300/90">
          {{ notice.detail }}
        </p>

        <div class="mt-2.5 space-y-1.5">
          <div
            v-for="command in notice.commands"
            :key="command"
            class="flex items-center gap-2"
          >
            <code
              class="flex-1 min-w-0 truncate rounded bg-amber-100 dark:bg-amber-950/40 px-2 py-1 font-mono text-xs text-amber-900 dark:text-amber-200"
              :title="command"
            >{{ command }}</code>
            <button
              class="shrink-0 rounded border border-amber-300 dark:border-amber-700/60 px-2 py-1 text-[10px] font-medium text-amber-700 dark:text-amber-300 hover:bg-amber-100 dark:hover:bg-amber-900/40 transition-colors"
              @click="copy(command)"
            >
              {{ copied === command ? 'Copied' : 'Copy' }}
            </button>
          </div>
        </div>

        <button
          v-if="notice.link"
          class="mt-2.5 inline-flex items-center gap-1 text-xs font-medium text-amber-800 dark:text-amber-300 hover:underline"
          @click="openUrl(notice.link.href)"
        >
          {{ notice.link.label }}
          <svg
            class="size-3"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M7 17 17 7M9 7h8v8" />
          </svg>
        </button>
      </div>
    </div>
  </div>
</template>
