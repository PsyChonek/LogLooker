<script lang="ts">
export interface DataTableColumn {
  key: string;
  label: string;
  // Default width in pixels. Omit for a flexible column that absorbs the
  // remaining space (there should be at least one per table).
  width?: number;
  // Resize floor in pixels (default 48)
  minWidth?: number;
  align?: 'left' | 'right' | 'center';
  // Extra classes for the header cell / body cells of this column
  headerClass?: string;
  cellClass?: string;
  // Drag-to-reorder and drag-to-resize are on by default; opt a column out here
  reorderable?: boolean;
  resizable?: boolean;
  // Header click cycles sort direction and shows an indicator (default off)
  sortable?: boolean;
  // Whether the column appears in the show/hide menu (default on)
  hideable?: boolean;
  // Renders cells with tabular figures so numbers and times line up
  numeric?: boolean;
  // The widest rendered cell acts as the column's width floor, so content is
  // never cropped. Only meaningful for columns whose cells do not wrap.
  fitContent?: boolean;
}

export interface SortState {
  // Column key; consumers map it to their own sort fields
  key: string;
  ascending: boolean;
}

interface TableLayout {
  order: string[];
  widths: Record<string, number>;
  hidden: string[];
}
</script>

<script setup lang="ts" generic="TRow">
import { computed, onBeforeUnmount, onMounted, onUpdated, ref, watch } from 'vue';
import { useColumnResize } from '@/composables/useColumnResize';
import BaseCheckbox from '@/components/BaseCheckbox.vue';
import Spinner from '@/components/Spinner.vue';

type ClassBinding = string | Record<string, boolean> | Array<string | Record<string, boolean>>;

const props = withDefaults(
  defineProps<{
    // Stable id; the saved column order, widths and hidden set are keyed on it
    tableId: string;
    columns: DataTableColumn[];
    rows: readonly TRow[];
    rowKey?: (row: TRow, index: number) => string | number;
    // A static class binding, or a function of the row that returns one
    rowClass?: ClassBinding | ((row: TRow, index: number) => ClassBinding);
    // Classes for the scroll container (height/overflow) and the <table> itself
    scrollClass?: string;
    tableClass?: string;
    // Vertical padding of body cells; horizontal padding is fixed at px-3
    cellPadding?: string;
    // When it returns true for a row, the #expansion slot renders a full-width row beneath it
    expanded?: (row: TRow, index: number) => boolean;
    // Controlled sort state; the table only renders indicators and proposes the
    // next state via update:sort - the consumer owns the actual ordering
    sort?: SortState | null;
    loading?: boolean;
    // Adds cursor-pointer and a hover accent rail to rows
    clickableRows?: boolean;
    // Built-in show/hide columns menu (default on); pass false to disable
    columnMenu?: boolean;
  }>(),
  {
    rowKey: undefined,
    rowClass: undefined,
    scrollClass: undefined,
    tableClass: undefined,
    cellPadding: 'py-1.5',
    expanded: undefined,
    sort: null,
    // Absent boolean props are cast to false, so the on-by-default flag needs an
    // explicit default
    columnMenu: true,
  },
);

const emit = defineEmits<{
  'row-click': [row: TRow, index: number, event: MouseEvent];
  'update:sort': [sort: SortState];
  scroll: [event: Event];
}>();

const STORAGE_PREFIX = 'loglooker.table.';

// Reconcile a saved layout with the current column set: keep the saved order for
// columns that still exist, append any new ones, and drop widths and hidden
// entries for columns that are gone or no longer hideable.
function loadLayout(): TableLayout {
  const keys = props.columns.map((c) => c.key);
  const byKey = new Map(props.columns.map((c) => [c.key, c]));
  try {
    const raw = localStorage.getItem(STORAGE_PREFIX + props.tableId);
    if (raw) {
      const saved = JSON.parse(raw) as Partial<TableLayout>;
      const order = Array.isArray(saved.order)
        ? saved.order.filter((k): k is string => keys.includes(k))
        : [];
      keys.forEach((k) => {
        if (!order.includes(k)) order.push(k);
      });
      const widths: Record<string, number> = {};
      if (saved.widths && typeof saved.widths === 'object') {
        for (const k of keys) {
          const w = (saved.widths as Record<string, unknown>)[k];
          // A non-resizable column's declared width is authoritative. Ignoring
          // an older saved width also makes a column stay fixed when a view is
          // changed from resizable to non-resizable.
          if (byKey.get(k)?.resizable !== false && typeof w === 'number' && w > 0) {
            widths[k] = w;
          }
        }
      }
      let hidden = Array.isArray(saved.hidden)
        ? saved.hidden.filter(
            (k): k is string => keys.includes(k) && byKey.get(k)?.hideable !== false,
          )
        : [];
      if (hidden.length >= keys.length) hidden = [];
      return { order, widths, hidden };
    }
  } catch {
    // fall through to defaults
  }
  return { order: keys, widths: {}, hidden: [] };
}

const layout = ref<TableLayout>(loadLayout());

// A fresh tableId (or a changed column set) means a different table entirely
watch(
  () => [props.tableId, props.columns.map((c) => c.key).join(',')],
  () => {
    layout.value = loadLayout();
  },
);

function saveLayout() {
  try {
    localStorage.setItem(STORAGE_PREFIX + props.tableId, JSON.stringify(layout.value));
  } catch {
    // ignore storage failures (private mode, quota)
  }
}

const columnMap = computed(() => new Map(props.columns.map((c) => [c.key, c])));

const orderedColumns = computed(() =>
  layout.value.order.map((k) => columnMap.value.get(k)).filter((c): c is DataTableColumn => !!c),
);

const visibleColumns = computed(() =>
  orderedColumns.value.filter((c) => !layout.value.hidden.includes(c.key)),
);

// Measured floors for fitContent columns; kept out of the saved layout so they
// always track the current rows
const contentWidths = ref<Record<string, number>>({});

function widthFor(col: DataTableColumn): number | undefined {
  const base = col.resizable === false ? col.width : (layout.value.widths[col.key] ?? col.width);
  const floor = col.fitContent ? contentWidths.value[col.key] : undefined;
  if (floor == null) return base;
  return base == null ? floor : Math.max(base, floor);
}

function alignClass(col: DataTableColumn): string {
  if (col.align === 'right') return 'text-right';
  if (col.align === 'center') return 'text-center';
  return 'text-left';
}

function rowClassFor(row: TRow, index: number): ClassBinding | undefined {
  return typeof props.rowClass === 'function' ? props.rowClass(row, index) : props.rowClass;
}

// --- Sort ---

function onHeaderClick(col: DataTableColumn) {
  if (!col.sortable) return;
  emit(
    'update:sort',
    props.sort?.key === col.key
      ? { key: col.key, ascending: !props.sort.ascending }
      : { key: col.key, ascending: true },
  );
}

function headerCursor(col: DataTableColumn): string {
  if (col.sortable) return 'cursor-pointer';
  if (col.reorderable !== false) return 'cursor-move';
  return '';
}

// --- Resize ---

const { resizingKey, startResize } = useColumnResize({
  setWidth: (key, width) => {
    layout.value.widths[key] = width;
  },
  minWidth: (key) => columnMap.value.get(key)?.minWidth ?? 48,
  onEnd: saveLayout,
});

// Widest rendered cell of the column at `idx`, as a column width: content plus
// the px-3 cell padding and column border, capped so a single huge line cannot
// blow the layout up. 0 when there is nothing to measure. A Range measures the
// laid-out content itself, so the answer does not depend on the column's
// current width the way scrollWidth does (which is floored at clientWidth).
function measureColumn(idx: number): number {
  const container = scrollRef.value;
  if (!container) return 0;
  const range = document.createRange();
  let widest = 0;
  container.querySelectorAll('tbody > tr').forEach((tr) => {
    // Full-width rows (expansion, empty, footer) span all columns; skip them
    if (tr.children.length !== visibleColumns.value.length) return;
    const inner = tr.children[idx]?.firstElementChild;
    if (inner instanceof HTMLElement) {
      range.selectNodeContents(inner);
      widest = Math.max(widest, range.getBoundingClientRect().width);
    }
  });
  return widest === 0 ? 0 : Math.min(Math.ceil(widest) + 26, 800);
}

// Double-click on the resize handle snaps the column to its widest rendered cell
function autoFit(key: string) {
  const idx = visibleColumns.value.findIndex((c) => c.key === key);
  if (idx < 0) return;
  const measured = measureColumn(idx);
  if (measured === 0) return;
  layout.value.widths[key] = Math.max(columnMap.value.get(key)?.minWidth ?? 48, measured);
  saveLayout();
}

// fitContent columns re-measure after every render, so they track appended rows
// and reformatted cells; writing an unchanged width does not re-trigger a render
function measureFitColumns() {
  visibleColumns.value.forEach((col, idx) => {
    if (!col.fitContent) return;
    const measured = measureColumn(idx);
    if (measured > 0) contentWidths.value[col.key] = measured;
  });
}
onMounted(measureFitColumns);
onUpdated(measureFitColumns);

// --- Reorder ---

const dragKey = ref<string | null>(null);
// Insertion point while a header drag is in progress: the dragged column lands
// before (after: false) or after the hovered column. Null when the drop would
// be a no-op or is refused, which also hides the insertion line.
const dropTarget = ref<{ key: string; after: boolean } | null>(null);
// x of the insertion line, relative to the component
const dropIndicatorX = ref<number | null>(null);

function onDragStart(col: DataTableColumn, event: DragEvent) {
  if (col.reorderable === false) return;
  dragKey.value = col.key;
  if (event.dataTransfer) {
    event.dataTransfer.effectAllowed = 'move';
    event.dataTransfer.setData('text/plain', col.key);
  }
}

function clearDrag() {
  dragKey.value = null;
  dropTarget.value = null;
  dropIndicatorX.value = null;
}

// The mouse position picks the insertion side: left half of a header inserts
// before it, right half after it
function onHeaderDragOver(col: DataTableColumn, colIndex: number, event: DragEvent) {
  if (!dragKey.value) return;
  // Pinned columns (reorderable: false) stay put; refuse a drop onto their slot
  if (col.reorderable === false) {
    dropTarget.value = null;
    dropIndicatorX.value = null;
    return;
  }
  const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
  const after = event.clientX >= rect.left + rect.width / 2;
  const dragIndex = visibleColumns.value.findIndex((c) => c.key === dragKey.value);
  const boundary = colIndex + (after ? 1 : 0);
  // The column would land exactly where it already sits
  if (boundary === dragIndex || boundary === dragIndex + 1) {
    dropTarget.value = null;
    dropIndicatorX.value = null;
    return;
  }
  if (event.dataTransfer) event.dataTransfer.dropEffect = 'move';
  dropTarget.value = { key: col.key, after };
  const originX = scrollRef.value?.getBoundingClientRect().left ?? 0;
  dropIndicatorX.value = (after ? rect.right : rect.left) - originX;
}

// While an insertion point is set, the whole table accepts the drop, so
// releasing over the body applies what the indicator shows
function onContainerDragOver(event: DragEvent) {
  if (dragKey.value && dropTarget.value) event.preventDefault();
}

function onDrop() {
  const from = dragKey.value;
  const target = dropTarget.value;
  clearDrag();
  if (!from || !target) return;
  const order = [...layout.value.order];
  const fromIndex = order.indexOf(from);
  if (fromIndex < 0) return;
  order.splice(fromIndex, 1);
  const targetIndex = order.indexOf(target.key);
  if (targetIndex < 0) return;
  order.splice(targetIndex + (target.after ? 1 : 0), 0, from);
  layout.value.order = order;
  saveLayout();
}

// --- Column menu ---

const menuOpen = ref(false);
// The dropdown teleports to <body> (the card's overflow-hidden would clip it),
// so it is positioned fixed, anchored to the trigger on open
const menuPos = ref({ top: 0, right: 0 });
const menuRef = ref<HTMLElement | null>(null);
const triggerRef = ref<HTMLElement | null>(null);

// The trigger docks flush into the header strip, so it must match the header
// row's height exactly; that height depends on the header content per table.
// ResizeObserver cannot observe table-internal boxes like <thead>, so the
// scroll container is observed and the header measured from it.
const theadRef = ref<HTMLElement | null>(null);
const scrollRef = ref<HTMLElement | null>(null);
const headerHeight = ref(0);
// The trigger is positioned over the scroll container, so it must stay clear of
// the container's vertical scrollbar or it blocks the scrollbar's top end
const scrollbarWidth = ref(0);
let scrollObserver: ResizeObserver | null = null;

function measureHeader() {
  headerHeight.value = theadRef.value?.getBoundingClientRect().height ?? 0;
  const el = scrollRef.value;
  scrollbarWidth.value = el ? el.offsetWidth - el.clientWidth : 0;
}

onMounted(() => {
  measureHeader();
  if (scrollRef.value) {
    scrollObserver = new ResizeObserver(measureHeader);
    scrollObserver.observe(scrollRef.value);
  }
});

onBeforeUnmount(() => {
  scrollObserver?.disconnect();
});

function toggleMenu() {
  if (menuOpen.value) {
    menuOpen.value = false;
    return;
  }
  const rect = triggerRef.value?.getBoundingClientRect();
  if (!rect) return;
  menuPos.value = { top: rect.bottom + 4, right: window.innerWidth - rect.right };
  menuOpen.value = true;
}

const hideableColumns = computed(() => orderedColumns.value.filter((c) => c.hideable !== false));

const showMenuButton = computed(() => props.columnMenu && hideableColumns.value.length > 0);

// A fixed-width final column is normally an icon/action rail. Center the menu
// trigger in that rail; for ordinary (resizable or flexible) final columns it
// remains docked to the table's right edge as before.
const menuRight = computed(() => {
  const last = visibleColumns.value[visibleColumns.value.length - 1];
  const fixedWidth = last?.resizable === false ? widthFor(last) : undefined;
  const centeredInset = fixedWidth == null ? 0 : Math.max(0, (fixedWidth - 28) / 2);
  return scrollbarWidth.value + centeredInset;
});

function toggleHidden(key: string) {
  const hidden = layout.value.hidden;
  const i = hidden.indexOf(key);
  if (i >= 0) hidden.splice(i, 1);
  // Never hide the last visible column
  else if (visibleColumns.value.length > 1) hidden.push(key);
  saveLayout();
}

function resetLayout() {
  try {
    localStorage.removeItem(STORAGE_PREFIX + props.tableId);
  } catch {
    // ignore
  }
  layout.value = { order: props.columns.map((c) => c.key), widths: {}, hidden: [] };
}

function onDocumentMousedown(event: MouseEvent) {
  const target = event.target as Node;
  if (menuRef.value?.contains(target) || triggerRef.value?.contains(target)) return;
  menuOpen.value = false;
}

function onDocumentKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') menuOpen.value = false;
}

// The fixed-position menu would drift from its anchor on scroll; close it instead
function onAnyScroll(event: Event) {
  if (menuRef.value && event.target instanceof Node && menuRef.value.contains(event.target)) return;
  menuOpen.value = false;
}

watch(menuOpen, (open) => {
  if (open) {
    document.addEventListener('mousedown', onDocumentMousedown);
    document.addEventListener('keydown', onDocumentKeydown);
    document.addEventListener('scroll', onAnyScroll, true);
  } else {
    document.removeEventListener('mousedown', onDocumentMousedown);
    document.removeEventListener('keydown', onDocumentKeydown);
    document.removeEventListener('scroll', onAnyScroll, true);
  }
});

onBeforeUnmount(() => {
  document.removeEventListener('mousedown', onDocumentMousedown);
  document.removeEventListener('keydown', onDocumentKeydown);
  document.removeEventListener('scroll', onAnyScroll, true);
});
</script>

<template>
  <div class="relative min-w-0 max-w-full">
    <div
      ref="scrollRef"
      class="w-full max-w-full overflow-auto"
      :class="scrollClass"
      @scroll.passive="emit('scroll', $event)"
      @dragover="onContainerDragOver"
      @drop.prevent="onDrop"
    >
      <!-- border-separate: collapsed borders are painted by the table grid, so
           they would not travel with the sticky header; separate borders belong
           to the cells and stick correctly -->
      <table
        class="w-full border-separate border-spacing-0 text-xs"
        :class="tableClass"
        style="table-layout: fixed"
      >
        <colgroup>
          <col
            v-for="col in visibleColumns"
            :key="col.key"
            :style="widthFor(col) != null ? { width: `${widthFor(col)}px` } : undefined"
          >
        </colgroup>
        <thead
          ref="theadRef"
          class="sticky top-0 z-10 bg-gray-50 dark:bg-gray-900"
        >
          <tr>
            <th
              v-for="(col, colIndex) in visibleColumns"
              :key="col.key"
              class="group/th relative border-b-2 border-b-gray-200 py-2 px-3 text-[10px] font-semibold uppercase tracking-wider select-none transition-colors dark:border-b-gray-700"
              :class="[
                alignClass(col),
                col.headerClass,
                headerCursor(col),
                sort?.key === col.key
                  ? 'text-blue-600 dark:text-blue-400'
                  : 'text-gray-500 dark:text-gray-400',
                col.sortable ? 'hover:text-gray-700 dark:hover:text-gray-200' : '',
                dragKey === col.key ? 'opacity-40' : '',
                colIndex < visibleColumns.length - 1
                  ? 'border-r border-r-gray-300 dark:border-r-gray-600'
                  : '',
              ]"
              :draggable="col.reorderable !== false"
              @dragstart="onDragStart(col, $event)"
              @dragend="clearDrag"
              @dragover.prevent="onHeaderDragOver(col, colIndex, $event)"
              @click="onHeaderClick(col)"
            >
              <svg
                v-if="col.reorderable !== false"
                class="mr-0.5 -ml-1 inline-block size-3 align-[-2px] text-gray-400 opacity-0 transition-opacity group-hover/th:opacity-70"
                viewBox="0 0 8 12"
                fill="currentColor"
                aria-hidden="true"
              >
                <circle
                  cx="2.5"
                  cy="2"
                  r="1"
                />
                <circle
                  cx="5.5"
                  cy="2"
                  r="1"
                />
                <circle
                  cx="2.5"
                  cy="6"
                  r="1"
                />
                <circle
                  cx="5.5"
                  cy="6"
                  r="1"
                />
                <circle
                  cx="2.5"
                  cy="10"
                  r="1"
                />
                <circle
                  cx="5.5"
                  cy="10"
                  r="1"
                />
              </svg>
              <slot
                :name="`header-${col.key}`"
                :col="col"
              >
                <span class="align-middle">{{ col.label }}</span>
              </slot>
              <svg
                v-if="sort?.key === col.key"
                class="ml-0.5 inline-block size-3 align-[-1px]"
                :class="sort.ascending ? '' : 'rotate-180'"
                viewBox="0 0 12 12"
                fill="none"
                stroke="currentColor"
                stroke-width="1.75"
                stroke-linecap="round"
                stroke-linejoin="round"
                aria-hidden="true"
              >
                <path d="M3 7.5 6 4.5l3 3" />
              </svg>
              <!-- Blue underline segment for the sorted column; sits over the header border -->
              <span
                v-if="sort?.key === col.key"
                class="absolute inset-x-0 -bottom-0.5 h-0.5 bg-blue-500"
              />
              <!-- Hit area for resizing; the always-visible line is the cell's border-r,
                   so the handle only paints a blue highlight over it on hover/drag.
                   It straddles the border (half in the next cell) so both sides of
                   the line are grabbable; the last column stays inside to avoid
                   widening the scroll area -->
              <span
                v-if="col.resizable !== false"
                class="group absolute top-0 z-10 flex h-full w-2.5 cursor-col-resize items-stretch"
                :class="
                  colIndex < visibleColumns.length - 1
                    ? '-right-[5px] justify-center'
                    : 'right-0 justify-end'
                "
                title="Drag to resize; double-click to fit content"
                @mousedown="startResize(col.key, $event)"
                @dblclick.stop="autoFit(col.key)"
                @click.stop
                @dragstart.prevent.stop
              >
                <span
                  class="h-full w-0.5 transition-colors"
                  :class="
                    resizingKey === col.key
                      ? 'bg-blue-400'
                      : 'bg-transparent group-hover:bg-blue-400'
                  "
                />
              </span>
            </th>
          </tr>
        </thead>
        <tbody>
          <template
            v-for="(row, index) in rows"
            :key="rowKey ? rowKey(row, index) : index"
          >
            <tr
              class="transition-colors hover:bg-gray-50 dark:hover:bg-gray-700/30"
              :class="[
                // box-shadow on <tr> renders fine in the Chromium-only WebView2 runtime
                clickableRows
                  ? 'cursor-pointer hover:shadow-[inset_2px_0_0_0_var(--color-blue-500)]'
                  : '',
                rowClassFor(row, index),
              ]"
              @click="emit('row-click', row, index, $event)"
            >
              <td
                v-for="(col, colIndex) in visibleColumns"
                :key="col.key"
                class="border-b border-b-gray-200 px-3 dark:border-b-gray-700"
                :class="[
                  cellPadding,
                  alignClass(col),
                  col.cellClass,
                  col.numeric ? 'tabular-nums' : '',
                  // The whole column fades while its header is being dragged
                  dragKey === col.key ? 'opacity-40' : '',
                  sort?.key === col.key ? 'bg-blue-500/5 dark:bg-blue-400/5' : '',
                  colIndex < visibleColumns.length - 1
                    ? 'border-r border-r-gray-100 dark:border-r-gray-700/40'
                    : '',
                ]"
              >
                <!-- Clip on an inner wrapper, not the cell; auto-fit also measures
                     this wrapper's scrollWidth -->
                <div class="overflow-hidden">
                  <slot
                    :name="col.key"
                    :row="row"
                    :index="index"
                    :col="col"
                  />
                </div>
              </td>
            </tr>
            <tr v-if="expanded && expanded(row, index)">
              <td
                :colspan="visibleColumns.length"
                class="border-b border-gray-200 dark:border-gray-700"
              >
                <slot
                  name="expansion"
                  :row="row"
                  :index="index"
                />
              </td>
            </tr>
          </template>
          <tr v-if="rows.length === 0">
            <td
              :colspan="visibleColumns.length"
              class="py-12 text-center"
            >
              <template v-if="loading">
                <Spinner
                  size="md"
                  class="mx-auto"
                />
                <div class="mt-2 text-xs text-gray-400">
                  Loading...
                </div>
              </template>
              <template v-else>
                <svg
                  class="mx-auto mb-2 size-8 text-gray-300 dark:text-gray-600"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.5"
                  stroke-linecap="round"
                  aria-hidden="true"
                >
                  <rect
                    x="3"
                    y="5"
                    width="18"
                    height="14"
                    rx="2"
                  />
                  <path d="M3 10h18M9 10v9" />
                </svg>
                <div class="text-xs text-gray-400">
                  <slot name="empty">
                    No data.
                  </slot>
                </div>
              </template>
            </td>
          </tr>
          <tr v-if="$slots.footer">
            <td
              :colspan="visibleColumns.length"
              class="border-0 p-0"
            >
              <slot name="footer" />
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <!-- Insertion line while a column header is being dragged; slides between
         column boundaries and marks where the column will land -->
    <div
      v-if="dropIndicatorX !== null"
      class="pointer-events-none absolute inset-y-0 z-30 w-0.5 -translate-x-1/2 bg-blue-500 transition-[left] duration-150 ease-out dark:bg-blue-400"
      :style="{ left: `${dropIndicatorX}px` }"
    >
      <span
        class="absolute top-0 left-1/2 block size-0 -translate-x-1/2 border-x-[5px] border-t-[6px] border-x-transparent border-t-blue-500 dark:border-t-blue-400"
      />
    </div>
    <!-- The trigger lives outside the scroll container so it stays pinned over the
         sticky header and the dropdown escapes the overflow clipping -->
    <button
      v-if="showMenuButton"
      ref="triggerRef"
      type="button"
      class="absolute top-0 z-20 flex w-7 items-center justify-center border-b-2 border-gray-200 bg-gray-50 text-gray-400 transition-colors hover:text-blue-500 dark:border-gray-700 dark:bg-gray-900"
      :style="{ height: `${headerHeight}px`, right: `${menuRight}px` }"
      title="Show / hide columns"
      @click="toggleMenu"
    >
      <svg
        class="size-3.5"
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        stroke-width="1.5"
        stroke-linecap="round"
        aria-hidden="true"
      >
        <rect
          x="1.5"
          y="2.5"
          width="13"
          height="11"
          rx="1.5"
        />
        <path d="M6 2.5v11M10.5 2.5v11" />
      </svg>
    </button>
    <Teleport to="body">
      <div
        v-if="menuOpen"
        ref="menuRef"
        class="fixed z-50 min-w-40 rounded-md border border-gray-200 bg-white p-1 shadow-lg dark:border-gray-700 dark:bg-gray-800"
        :style="{ top: `${menuPos.top}px`, right: `${menuPos.right}px` }"
      >
        <div
          v-for="col in hideableColumns"
          :key="col.key"
          class="rounded px-2 py-1 whitespace-nowrap hover:bg-gray-50 dark:hover:bg-gray-700"
        >
          <BaseCheckbox
            :model-value="!layout.hidden.includes(col.key)"
            @update:model-value="toggleHidden(col.key)"
          >
            <span class="text-xs text-gray-700 dark:text-gray-300">{{ col.label }}</span>
          </BaseCheckbox>
        </div>
        <div class="my-1 border-t border-gray-200 dark:border-gray-700" />
        <button
          type="button"
          class="w-full rounded px-2 py-1 text-left text-xs text-gray-500 hover:bg-gray-50 hover:text-gray-700 dark:text-gray-400 dark:hover:bg-gray-700 dark:hover:text-gray-200"
          @click="resetLayout"
        >
          Reset layout
        </button>
      </div>
    </Teleport>
    <div
      v-if="loading && rows.length > 0"
      class="absolute inset-0 z-20 flex items-start justify-center bg-white/40 pt-16 backdrop-blur-[1px] dark:bg-gray-900/40"
    >
      <div
        class="flex items-center gap-2 rounded-md border border-gray-200 bg-white/95 px-3 py-1.5 text-xs text-gray-600 shadow-md dark:border-gray-700 dark:bg-gray-800/95 dark:text-gray-300"
      >
        <Spinner size="sm" />
        Loading...
      </div>
    </div>
  </div>
</template>
