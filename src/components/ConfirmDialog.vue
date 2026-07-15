<script setup lang="ts">
defineProps<{
  visible: boolean;
  message: string;
}>();

const emit = defineEmits<{
  confirm: [];
  cancel: [];
}>();
</script>

<template>
  <Teleport to="body">
    <div
      v-if="visible"
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/30 backdrop-blur-sm"
      @click.self="emit('cancel')"
    >
      <div class="bg-white dark:bg-gray-800 rounded-lg shadow-xl p-6 max-w-sm w-full mx-4">
        <p class="text-gray-700 dark:text-gray-300 mb-6">
          {{ message }}
        </p>
        <div class="flex justify-end gap-3">
          <button
            class="px-4 py-2 text-sm text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 border border-gray-300 dark:border-gray-600 rounded-md"
            @click="emit('cancel')"
          >
            Cancel
          </button>
          <button
            class="px-4 py-2 text-sm text-white bg-red-500 hover:bg-red-600 rounded-md"
            @click="emit('confirm')"
          >
            Delete
          </button>
        </div>
      </div>
    </div>
  </Teleport>
</template>
