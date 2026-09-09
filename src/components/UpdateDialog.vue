<script setup lang="ts">
import { invoke, isTauri } from '@tauri-apps/api/core';
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { useUpdates } from '@/composables/useUpdates';

const { t, te } = useI18n();
const { state, check, dismiss, install } = useUpdates(invoke);
const dialog = ref<HTMLDialogElement | null>(null);
let startupTimer: ReturnType<typeof setTimeout> | undefined;
let previousFocus: HTMLElement | null = null;

const errorMessage = computed(() => {
  const key = `updates.errors.${state.error}`;
  return t(te(key) ? key : 'updates.errors.checkFailed');
});

watch(
  () => state.visible,
  (visible) => {
    if (visible) {
      previousFocus = document.activeElement as HTMLElement | null;
      dialog.value?.showModal();
    } else {
      dialog.value?.close();
      if (previousFocus?.isConnected) previousFocus.focus();
    }
  },
  { flush: 'post' },
);

function checkManually() {
  clearTimeout(startupTimer);
  return check();
}

onMounted(() => {
  if (isTauri()) startupTimer = setTimeout(() => void check(false), 3000);
});
onBeforeUnmount(() => clearTimeout(startupTimer));

defineExpose({ check: checkManually });
</script>

<template>
  <Teleport to="body">
    <dialog
      ref="dialog"
      aria-labelledby="update-title"
      aria-describedby="update-status"
      class="m-auto w-[calc(100%-2rem)] max-w-md rounded-lg border border-gray-200 bg-white p-6 text-gray-800 shadow-xl backdrop:bg-black/40 dark:border-gray-700 dark:bg-gray-800 dark:text-gray-100"
      @cancel.prevent="dismiss"
    >
      <h2
        id="update-title"
        class="mb-4 text-lg font-semibold"
      >
        {{ t(state.result?.updateAvailable ? 'updates.available' : 'updates.title') }}
      </h2>
      <div
        id="update-status"
        role="status"
        aria-live="polite"
        class="space-y-2 text-sm"
      >
        <p v-if="state.checking">
          {{ t('updates.checking') }}
        </p>
        <template v-else-if="state.result">
          <p>{{ t('updates.current', { version: state.result.currentVersion }) }}</p>
          <p v-if="state.result.updateAvailable">
            {{ t('updates.latest', { version: state.result.latestVersion }) }}
          </p>
          <p v-else>
            {{ t('updates.upToDate') }}
          </p>
          <p
            v-if="state.result.updateAvailable"
            class="pt-2"
          >
            {{ t('updates.instructions') }}
          </p>
        </template>
        <p v-if="state.installing">
          {{ t('updates.installing') }}
        </p>
      </div>
      <p
        v-if="state.error"
        role="alert"
        class="mt-3 text-sm text-red-700 dark:text-red-300"
      >
        {{ errorMessage }}
      </p>
      <div class="mt-6 flex justify-end gap-3">
        <button
          autofocus
          class="rounded-md border border-gray-300 px-4 py-2 text-sm focus-visible:outline-2 focus-visible:outline-blue-600 disabled:opacity-50 dark:border-gray-600"
          :disabled="state.installing"
          @click="dismiss"
        >
          {{ t(state.result?.updateAvailable ? 'updates.later' : 'updates.close') }}
        </button>
        <button
          v-if="state.error"
          class="rounded-md bg-blue-600 px-4 py-2 text-sm text-white hover:bg-blue-700 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-blue-600 disabled:opacity-50"
          :disabled="state.checking || state.installing"
          @click="check()"
        >
          {{ t('updates.retry') }}
        </button>
        <button
          v-else-if="state.result?.updateAvailable"
          class="rounded-md bg-blue-600 px-4 py-2 text-sm text-white hover:bg-blue-700 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-blue-600 disabled:opacity-50"
          :disabled="state.installing || state.checking"
          @click="install"
        >
          {{ t(state.installing ? 'updates.installing' : 'updates.install') }}
        </button>
      </div>
    </dialog>
  </Teleport>
</template>
