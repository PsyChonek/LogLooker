import { computed, ref, watch, type Ref } from 'vue';

const STORAGE_PREFIX = 'loglooker.pagination.';

export const PAGE_SIZE_OPTIONS = [50, 100, 250, 500, 1000] as const;

function loadPageSize(storageKey: string | undefined, fallback: number): number {
  if (!storageKey) return fallback;
  try {
    const raw = localStorage.getItem(STORAGE_PREFIX + storageKey);
    const size = raw === null ? NaN : Number(raw);
    return PAGE_SIZE_OPTIONS.includes(size as (typeof PAGE_SIZE_OPTIONS)[number])
      ? size
      : fallback;
  } catch {
    return fallback;
  }
}

/*
 * Client-side paging for a table.
 *
 * A few thousand rows in the DOM at once make scrolling, sorting and even
 * typing in the filter box crawl, so views slice their sorted rows through
 * this and render one page at a time. The page is clamped whenever the row
 * count shrinks (a delete on the last page, a narrower filter), so the table
 * never ends up showing an empty page.
 */
export function usePagination<T>(
  rows: Ref<readonly T[]>,
  options: { storageKey?: string; defaultSize?: number } = {},
) {
  const { storageKey, defaultSize = 100 } = options;

  const page = ref(1);
  const pageSize = ref(loadPageSize(storageKey, defaultSize));

  const total = computed(() => rows.value.length);
  const pageCount = computed(() => Math.max(1, Math.ceil(total.value / pageSize.value)));

  watch(pageCount, (count) => {
    if (page.value > count) page.value = count;
  });

  watch(pageSize, (size) => {
    if (!storageKey) return;
    try {
      localStorage.setItem(STORAGE_PREFIX + storageKey, String(size));
    } catch {
      // ignore storage failures (private mode, quota)
    }
  });

  // Keep the first row of the current page visible when the page size changes,
  // so switching from 100 to 250 does not throw the user back to the top
  function setPageSize(size: number) {
    const firstRow = (page.value - 1) * pageSize.value;
    pageSize.value = size;
    page.value = Math.floor(firstRow / size) + 1;
  }

  const start = computed(() => (page.value - 1) * pageSize.value);
  const pagedRows = computed(() => rows.value.slice(start.value, start.value + pageSize.value));

  function reset() {
    page.value = 1;
  }

  return { page, pageSize, setPageSize, pageCount, total, start, pagedRows, reset };
}
