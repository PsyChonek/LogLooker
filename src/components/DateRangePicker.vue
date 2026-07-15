<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';

const from = defineModel<string>('from', { required: true });
const to = defineModel<string>('to', { required: true });

const WEEKDAYS = ['Mo', 'Tu', 'We', 'Th', 'Fr', 'Sa', 'Su'];
const MONTHS = [
  'January',
  'February',
  'March',
  'April',
  'May',
  'June',
  'July',
  'August',
  'September',
  'October',
  'November',
  'December',
];

function toKey(date: Date): string {
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  return `${date.getFullYear()}-${month}-${day}`;
}

function parseKey(key: string): Date {
  const [year, month, day] = key.split('-').map(Number);
  return new Date(year, month - 1, day);
}

function daysAgo(count: number): Date {
  const date = new Date();
  date.setDate(date.getDate() - count);
  return date;
}

function shortLabel(key: string): string {
  const date = parseKey(key);
  return `${date.getDate()} ${MONTHS[date.getMonth()].slice(0, 3)}`;
}

interface Preset {
  label: string;
  from: () => string;
  to: () => string;
}

const PRESETS: Preset[] = [
  { label: 'Today', from: () => toKey(new Date()), to: () => toKey(new Date()) },
  { label: 'Yesterday', from: () => toKey(daysAgo(1)), to: () => toKey(daysAgo(1)) },
  { label: 'Last 3 days', from: () => toKey(daysAgo(2)), to: () => toKey(new Date()) },
  { label: 'Last 7 days', from: () => toKey(daysAgo(6)), to: () => toKey(new Date()) },
  { label: 'Last 14 days', from: () => toKey(daysAgo(13)), to: () => toKey(new Date()) },
  { label: 'Last 30 days', from: () => toKey(daysAgo(29)), to: () => toKey(new Date()) },
  {
    label: 'This month',
    from: () => {
      const date = new Date();
      return toKey(new Date(date.getFullYear(), date.getMonth(), 1));
    },
    to: () => toKey(new Date()),
  },
];

const open = ref(false);
const root = ref<HTMLElement | null>(null);
const cursor = ref(new Date());
const pendingStart = ref<string | null>(null);
const hovered = ref<string | null>(null);
const todayKey = toKey(new Date());

const activePreset = computed(
  () => PRESETS.find((p) => p.from() === from.value && p.to() === to.value)?.label ?? null,
);

const triggerLabel = computed(() => {
  if (activePreset.value) return activePreset.value;
  if (from.value === to.value) return shortLabel(from.value);
  return `${shortLabel(from.value)} - ${shortLabel(to.value)}`;
});

const dayCount = computed(() => {
  const span = parseKey(to.value).getTime() - parseKey(from.value).getTime();
  return Math.round(span / 86_400_000) + 1;
});

// Selection preview while the second click is still pending.
const rangeStart = computed(() => (pendingStart.value ? pendingStart.value : from.value));
const rangeEnd = computed(() => {
  if (!pendingStart.value) return to.value;
  return hovered.value ?? pendingStart.value;
});
const previewFrom = computed(() =>
  rangeStart.value <= rangeEnd.value ? rangeStart.value : rangeEnd.value,
);
const previewTo = computed(() =>
  rangeStart.value <= rangeEnd.value ? rangeEnd.value : rangeStart.value,
);

interface Cell {
  key: string;
  day: number;
  outside: boolean;
}

const cells = computed<Cell[]>(() => {
  const year = cursor.value.getFullYear();
  const month = cursor.value.getMonth();
  const first = new Date(year, month, 1);
  const offset = (first.getDay() + 6) % 7; // Monday-first grid
  const start = new Date(year, month, 1 - offset);
  return Array.from({ length: 42 }, (_, i) => {
    const date = new Date(start.getFullYear(), start.getMonth(), start.getDate() + i);
    return { key: toKey(date), day: date.getDate(), outside: date.getMonth() !== month };
  });
});

function inRange(key: string): boolean {
  return key >= previewFrom.value && key <= previewTo.value;
}

function isEdge(key: string): boolean {
  return key === previewFrom.value || key === previewTo.value;
}

// The connecting bar spans the whole cell so selected days form one continuous strip.
function barClass(key: string): string[] {
  if (!inRange(key) || previewFrom.value === previewTo.value) {
    return ['rounded-md', 'hover:bg-gray-100', 'dark:hover:bg-gray-700'];
  }
  const classes = ['bg-blue-50', 'dark:bg-blue-500/15'];
  if (key === previewFrom.value) classes.push('rounded-l-md');
  if (key === previewTo.value) classes.push('rounded-r-md');
  return classes;
}

function pickDay(key: string) {
  if (pendingStart.value === null) {
    pendingStart.value = key;
    hovered.value = key;
    return;
  }
  const start = pendingStart.value;
  from.value = start <= key ? start : key;
  to.value = start <= key ? key : start;
  pendingStart.value = null;
  hovered.value = null;
  open.value = false;
}

function applyPreset(preset: Preset) {
  from.value = preset.from();
  to.value = preset.to();
  pendingStart.value = null;
  open.value = false;
}

function shiftMonth(delta: number) {
  cursor.value = new Date(cursor.value.getFullYear(), cursor.value.getMonth() + delta, 1);
}

function toggle() {
  open.value = !open.value;
}

watch(open, (isOpen) => {
  if (isOpen) {
    cursor.value = parseKey(from.value);
    pendingStart.value = null;
    hovered.value = null;
  }
});

function onPointerDown(event: MouseEvent) {
  if (open.value && root.value && !root.value.contains(event.target as Node)) {
    open.value = false;
  }
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape' && open.value) {
    open.value = false;
  }
}

onMounted(() => {
  document.addEventListener('mousedown', onPointerDown);
  document.addEventListener('keydown', onKeydown);
});

onBeforeUnmount(() => {
  document.removeEventListener('mousedown', onPointerDown);
  document.removeEventListener('keydown', onKeydown);
});
</script>

<template>
  <div
    ref="root"
    class="relative"
  >
    <button
      type="button"
      class="flex items-center gap-2 px-2.5 py-1.5 text-xs font-medium rounded-md border bg-white dark:bg-gray-800 text-gray-700 dark:text-gray-200 transition-colors"
      :class="
        open
          ? 'border-blue-500 ring-2 ring-blue-500/20'
          : 'border-gray-300 dark:border-gray-600 hover:border-blue-400 dark:hover:border-blue-500'
      "
      @click="toggle"
    >
      <svg
        class="size-3.5 text-gray-400"
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        stroke-width="1.5"
        stroke-linecap="round"
      >
        <rect
          x="2"
          y="3"
          width="12"
          height="11"
          rx="2"
        />
        <path d="M2 6.5h12M5.5 1.5v2M10.5 1.5v2" />
      </svg>
      {{ triggerLabel }}
      <span class="text-[10px] text-gray-400">{{ dayCount }}d</span>
    </button>

    <div
      v-if="open"
      class="absolute right-0 top-full mt-2 z-30 flex rounded-xl border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800 shadow-xl shadow-black/10 dark:shadow-black/40 overflow-hidden"
    >
      <div
        class="flex flex-col gap-0.5 p-2 w-32 border-r border-gray-100 dark:border-gray-700 bg-gray-50/60 dark:bg-gray-900/40"
      >
        <button
          v-for="preset in PRESETS"
          :key="preset.label"
          type="button"
          class="px-2 py-1.5 text-left text-xs rounded-md transition-colors"
          :class="
            activePreset === preset.label
              ? 'bg-blue-500 text-white font-medium'
              : 'text-gray-600 dark:text-gray-400 hover:bg-gray-200/70 dark:hover:bg-gray-700 hover:text-gray-900 dark:hover:text-gray-100'
          "
          @click="applyPreset(preset)"
        >
          {{ preset.label }}
        </button>
      </div>

      <div class="p-3 w-64">
        <div class="flex items-center justify-between mb-2">
          <button
            type="button"
            class="grid place-items-center size-6 rounded-md text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 hover:text-gray-700 dark:hover:text-gray-200 transition-colors"
            @click="shiftMonth(-1)"
          >
            <svg
              class="size-3.5"
              viewBox="0 0 16 16"
              fill="none"
              stroke="currentColor"
              stroke-width="1.75"
              stroke-linecap="round"
              stroke-linejoin="round"
            >
              <path d="M10 3L5 8l5 5" />
            </svg>
          </button>
          <span class="text-xs font-semibold text-gray-700 dark:text-gray-200">
            {{ MONTHS[cursor.getMonth()] }} {{ cursor.getFullYear() }}
          </span>
          <button
            type="button"
            class="grid place-items-center size-6 rounded-md text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 hover:text-gray-700 dark:hover:text-gray-200 transition-colors"
            @click="shiftMonth(1)"
          >
            <svg
              class="size-3.5"
              viewBox="0 0 16 16"
              fill="none"
              stroke="currentColor"
              stroke-width="1.75"
              stroke-linecap="round"
              stroke-linejoin="round"
            >
              <path d="M6 3l5 5-5 5" />
            </svg>
          </button>
        </div>

        <div class="grid grid-cols-7 mb-1">
          <span
            v-for="weekday in WEEKDAYS"
            :key="weekday"
            class="grid place-items-center h-6 text-[10px] font-medium text-gray-400"
          >
            {{ weekday }}
          </span>
        </div>

        <div
          class="grid grid-cols-7 gap-y-0.5"
          @mouseleave="hovered = null"
        >
          <button
            v-for="cell in cells"
            :key="cell.key"
            type="button"
            class="grid place-items-center h-8 text-xs text-gray-700 dark:text-gray-300 transition-colors"
            :class="[
              barClass(cell.key),
              inRange(cell.key) && !isEdge(cell.key) ? 'text-blue-700 dark:text-blue-300' : '',
              cell.outside && !inRange(cell.key) ? 'text-gray-300 dark:text-gray-600' : '',
            ]"
            @mouseenter="hovered = cell.key"
            @click="pickDay(cell.key)"
          >
            <span
              class="grid place-items-center size-7 rounded-md transition-colors"
              :class="[
                isEdge(cell.key) ? 'bg-blue-500 text-white font-semibold shadow-sm' : '',
                !isEdge(cell.key) && cell.key === todayKey
                  ? 'ring-1 ring-inset ring-blue-400 font-semibold'
                  : '',
              ]"
            >
              {{ cell.day }}
            </span>
          </button>
        </div>

        <div
          class="mt-2 pt-2 border-t border-gray-100 dark:border-gray-700 text-[10px] text-gray-400 text-center"
        >
          <template v-if="pendingStart">
            Pick the end of the range
          </template>
          <template v-else>
            {{ from }} to {{ to }}
          </template>
        </div>
      </div>
    </div>
  </div>
</template>
