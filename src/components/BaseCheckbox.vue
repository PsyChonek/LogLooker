<script setup lang="ts">
const model = defineModel<boolean>({ default: false });

withDefaults(
  defineProps<{
    disabled?: boolean;
    indeterminate?: boolean;
  }>(),
  { disabled: false, indeterminate: false },
);
</script>

<template>
  <label
    class="group inline-flex items-center gap-2 select-none"
    :class="disabled ? 'cursor-not-allowed opacity-40' : 'cursor-pointer'"
  >
    <input
      v-model="model"
      type="checkbox"
      class="peer sr-only"
      :disabled="disabled"
    >
    <span
      class="relative grid place-items-center size-4 shrink-0 rounded-[5px] border transition-all duration-150 peer-focus-visible:ring-2 peer-focus-visible:ring-blue-500/40 peer-focus-visible:ring-offset-1 peer-focus-visible:ring-offset-white dark:peer-focus-visible:ring-offset-gray-900"
      :class="[
        model || indeterminate
          ? 'bg-blue-500 border-blue-500 shadow-sm shadow-blue-500/30'
          : 'bg-white dark:bg-gray-900 border-gray-300 dark:border-gray-600',
        !disabled && !model && !indeterminate
          ? 'group-hover:border-blue-400 dark:group-hover:border-blue-500'
          : '',
      ]"
    >
      <svg
        v-if="indeterminate && !model"
        class="size-3 text-white"
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        stroke-width="2.5"
        stroke-linecap="round"
      >
        <path d="M4 8h8" />
      </svg>
      <svg
        v-else
        class="size-3 text-white transition-transform duration-150"
        :class="model ? 'scale-100' : 'scale-0'"
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        stroke-width="2.5"
        stroke-linecap="round"
        stroke-linejoin="round"
      >
        <path d="M3.5 8.5l3 3 6-7" />
      </svg>
    </span>
    <span
      v-if="$slots.default"
      class="leading-none"
    >
      <slot />
    </span>
  </label>
</template>
