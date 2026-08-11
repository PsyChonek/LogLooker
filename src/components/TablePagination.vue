<script setup lang="ts">
import { computed } from 'vue';
import BaseSelect from '@/components/BaseSelect.vue';
import { PAGE_SIZE_OPTIONS } from '@/composables/usePagination';

/*
 * Pager bar for a DataTable, driven by usePagination. It renders below the
 * table and owns nothing itself: the page and the page size live in the view,
 * which slices the rows.
 */

const page = defineModel<number>('page', { required: true });

const props = defineProps<{
  pageCount: number;
  pageSize: number;
  total: number;
  // Index of the first row on the current page, zero based
  start: number;
  // Plural noun for the counter, e.g. "files"
  label?: string;
}>();

const emit = defineEmits<{ 'update:pageSize': [size: number] }>();

const sizeOptions = PAGE_SIZE_OPTIONS.map((size) => ({ value: size, label: String(size) }));

const firstShown = computed(() => (props.total === 0 ? 0 : props.start + 1));
const lastShown = computed(() => Math.min(props.start + props.pageSize, props.total));

// Page buttons: always the first and last page, plus a window around the
// current one, with gaps collapsed into an ellipsis
const pageItems = computed<(number | 'gap')[]>(() => {
  const count = props.pageCount;
  if (count <= 7) return Array.from({ length: count }, (_, i) => i + 1);
  const current = page.value;
  const pages = new Set([1, count, current, current - 1, current + 1]);
  if (current <= 3) [2, 3, 4].forEach((p) => pages.add(p));
  if (current >= count - 2) [count - 3, count - 2, count - 1].forEach((p) => pages.add(p));
  const sorted = [...pages].filter((p) => p >= 1 && p <= count).sort((a, b) => a - b);
  const items: (number | 'gap')[] = [];
  sorted.forEach((p, i) => {
    if (i > 0 && p - sorted[i - 1] > 1) items.push('gap');
    items.push(p);
  });
  return items;
});

function go(to: number) {
  page.value = Math.min(Math.max(1, to), props.pageCount);
}

const numberFormat = new Intl.NumberFormat();
const formatCount = (value: number) => numberFormat.format(value);
</script>

<template>
  <div
    class="flex flex-wrap items-center gap-x-4 gap-y-2 border-t border-gray-200 bg-gray-50 px-3 py-2 text-xs text-gray-600 dark:border-gray-700 dark:bg-gray-900 dark:text-gray-400"
  >
    <label class="flex items-center gap-1.5">
      <span>Rows per page</span>
      <BaseSelect
        :model-value="pageSize"
        :options="sizeOptions"
        size="sm"
        @update:model-value="emit('update:pageSize', $event)"
      />
    </label>

    <span class="tabular-nums">
      {{ formatCount(firstShown) }}-{{ formatCount(lastShown) }} of {{ formatCount(total) }}
      {{ label ?? 'rows' }}
    </span>

    <nav
      v-if="pageCount > 1"
      class="ml-auto flex items-center gap-1"
      aria-label="Pagination"
    >
      <button
        type="button"
        class="rounded px-2 py-1 transition-colors enabled:hover:bg-gray-200 disabled:opacity-40 dark:enabled:hover:bg-gray-700"
        :disabled="page <= 1"
        aria-label="First page"
        @click="go(1)"
      >
        &laquo;
      </button>
      <button
        type="button"
        class="rounded px-2 py-1 transition-colors enabled:hover:bg-gray-200 disabled:opacity-40 dark:enabled:hover:bg-gray-700"
        :disabled="page <= 1"
        aria-label="Previous page"
        @click="go(page - 1)"
      >
        &lsaquo;
      </button>

      <template
        v-for="(item, index) in pageItems"
        :key="`${item}-${index}`"
      >
        <span
          v-if="item === 'gap'"
          class="px-1 text-gray-400 select-none"
          aria-hidden="true"
        >...</span>
        <button
          v-else
          type="button"
          class="min-w-7 rounded px-2 py-1 tabular-nums transition-colors"
          :class="
            item === page
              ? 'bg-blue-500 font-semibold text-white'
              : 'hover:bg-gray-200 dark:hover:bg-gray-700'
          "
          :aria-label="`Page ${item}`"
          :aria-current="item === page ? 'page' : undefined"
          @click="go(item)"
        >
          {{ item }}
        </button>
      </template>

      <button
        type="button"
        class="rounded px-2 py-1 transition-colors enabled:hover:bg-gray-200 disabled:opacity-40 dark:enabled:hover:bg-gray-700"
        :disabled="page >= pageCount"
        aria-label="Next page"
        @click="go(page + 1)"
      >
        &rsaquo;
      </button>
      <button
        type="button"
        class="rounded px-2 py-1 transition-colors enabled:hover:bg-gray-200 disabled:opacity-40 dark:enabled:hover:bg-gray-700"
        :disabled="page >= pageCount"
        aria-label="Last page"
        @click="go(pageCount)"
      >
        &raquo;
      </button>
    </nav>
  </div>
</template>
