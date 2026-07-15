<script setup lang="ts">
import { computed } from 'vue';

const props = withDefaults(defineProps<{ size?: 'sm' | 'md' }>(), { size: 'md' });

const sizeClass = computed(() => (props.size === 'sm' ? 'w-3 h-3 border-2' : 'w-5 h-5 border-2'));
</script>

<template>
  <span
    class="inline-block shrink-0 rounded-full border-gray-300 dark:border-gray-600 border-t-blue-500 dark:border-t-blue-400 animate-spin"
    :class="sizeClass"
  />
</template>
