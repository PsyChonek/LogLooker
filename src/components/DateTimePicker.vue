<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';

const value = defineModel<string>({ required: true });

const props = withDefaults(
  defineProps<{
    ariaLabel?: string;
    align?: 'left' | 'right';
    utc?: boolean;
  }>(),
  {
    ariaLabel: 'Vybrat datum a čas',
    align: 'left',
    utc: false,
  },
);

const WEEKDAYS = ['Po', 'Út', 'St', 'Čt', 'Pá', 'So', 'Ne'];
const MONTHS = [
  'leden',
  'únor',
  'březen',
  'duben',
  'květen',
  'červen',
  'červenec',
  'srpen',
  'září',
  'říjen',
  'listopad',
  'prosinec',
];

const open = ref(false);
const root = ref<HTMLElement | null>(null);
const cursor = ref(new Date());
const draftDate = ref('');
const draftHour = ref(0);
const draftMinute = ref(0);

const pad = (part: number) => String(part).padStart(2, '0');

function dateKey(date: Date): string {
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

function parseValue(current: string): { date: string; hour: number; minute: number } | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(current);
  if (!match) return null;
  return {
    date: `${match[1]}-${match[2]}-${match[3]}`,
    hour: Number(match[4]),
    minute: Number(match[5]),
  };
}

function dateFromKey(key: string): Date {
  const [year, month, day] = key.split('-').map(Number);
  return new Date(year, month - 1, day);
}

function currentParts(): { date: string; hour: number; minute: number } {
  const now = new Date();
  if (!props.utc) {
    return { date: dateKey(now), hour: now.getHours(), minute: now.getMinutes() };
  }
  return {
    date: `${now.getUTCFullYear()}-${pad(now.getUTCMonth() + 1)}-${pad(now.getUTCDate())}`,
    hour: now.getUTCHours(),
    minute: now.getUTCMinutes(),
  };
}

const displayValue = computed(() => {
  const parsed = parseValue(value.value);
  if (!parsed) return 'dd. mm. rrrr hh:mm';
  const [year, month, day] = parsed.date.split('-');
  return `${day}. ${month}. ${year} ${pad(parsed.hour)}:${pad(parsed.minute)}`;
});

interface Cell {
  key: string;
  day: number;
  outside: boolean;
}

const cells = computed<Cell[]>(() => {
  const year = cursor.value.getFullYear();
  const month = cursor.value.getMonth();
  const first = new Date(year, month, 1);
  const mondayOffset = (first.getDay() + 6) % 7;
  const start = new Date(year, month, 1 - mondayOffset);
  return Array.from({ length: 42 }, (_, index) => {
    const date = new Date(start.getFullYear(), start.getMonth(), start.getDate() + index);
    return {
      key: dateKey(date),
      day: date.getDate(),
      outside: date.getMonth() !== month,
    };
  });
});

function beginEdit() {
  const now = currentParts();
  const parsed = parseValue(value.value);
  draftDate.value = parsed?.date ?? now.date;
  draftHour.value = parsed?.hour ?? now.hour;
  draftMinute.value = parsed?.minute ?? now.minute;
  cursor.value = dateFromKey(draftDate.value);
}

function toggle() {
  if (!open.value) beginEdit();
  open.value = !open.value;
}

function shiftMonth(delta: number) {
  cursor.value = new Date(cursor.value.getFullYear(), cursor.value.getMonth() + delta, 1);
}

function pickDay(key: string) {
  draftDate.value = key;
  const picked = dateFromKey(key);
  if (picked.getMonth() !== cursor.value.getMonth()) cursor.value = picked;
}

function pickNow() {
  const now = currentParts();
  draftDate.value = now.date;
  draftHour.value = now.hour;
  draftMinute.value = now.minute;
  const date = dateFromKey(now.date);
  cursor.value = new Date(date.getFullYear(), date.getMonth(), 1);
}

function apply() {
  value.value = `${draftDate.value}T${pad(draftHour.value)}:${pad(draftMinute.value)}`;
  open.value = false;
}

function clear() {
  value.value = '';
  open.value = false;
}

function onPointerDown(event: MouseEvent) {
  if (open.value && root.value && !root.value.contains(event.target as Node)) open.value = false;
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') open.value = false;
}

watch(value, () => {
  if (open.value) beginEdit();
});

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
  <div ref="root" class="relative">
    <button
      type="button"
      :aria-label="props.ariaLabel"
      :aria-expanded="open"
      class="flex min-w-48 items-center justify-between gap-2 rounded-md border bg-white px-2 py-1 text-left text-xs text-gray-700 transition-colors dark:bg-gray-800 dark:text-gray-200"
      :class="
        open
          ? 'border-blue-500 ring-2 ring-blue-500/20'
          : 'border-gray-300 hover:border-blue-400 dark:border-gray-600 dark:hover:border-blue-500'
      "
      @click="toggle"
    >
      <span :class="value ? '' : 'text-gray-400'">{{ displayValue }}</span>
      <svg
        class="size-3.5 shrink-0 text-gray-400"
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        stroke-width="1.5"
        stroke-linecap="round"
      >
        <rect x="2" y="3" width="12" height="11" rx="2" />
        <path d="M2 6.5h12M5.5 1.5v2M10.5 1.5v2" />
      </svg>
    </button>

    <div
      v-if="open"
      class="absolute top-full z-40 mt-2 w-72 rounded-xl border border-gray-200 bg-white p-3 shadow-xl shadow-black/10 dark:border-gray-700 dark:bg-gray-800 dark:shadow-black/40"
      :class="align === 'right' ? 'right-0' : 'left-0'"
    >
      <div class="mb-2 flex items-center justify-between">
        <button
          type="button"
          aria-label="Předchozí měsíc"
          class="grid size-7 place-items-center rounded-md text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-700 dark:hover:bg-gray-700 dark:hover:text-gray-200"
          @click="shiftMonth(-1)"
        >
          <span aria-hidden="true">‹</span>
        </button>
        <span class="text-xs font-semibold text-gray-700 dark:text-gray-200">
          {{ MONTHS[cursor.getMonth()] }} {{ cursor.getFullYear() }}
        </span>
        <button
          type="button"
          aria-label="Následující měsíc"
          class="grid size-7 place-items-center rounded-md text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-700 dark:hover:bg-gray-700 dark:hover:text-gray-200"
          @click="shiftMonth(1)"
        >
          <span aria-hidden="true">›</span>
        </button>
      </div>

      <div class="mb-1 grid grid-cols-7">
        <span
          v-for="weekday in WEEKDAYS"
          :key="weekday"
          class="grid h-6 place-items-center text-[10px] font-medium text-gray-400"
        >
          {{ weekday }}
        </span>
      </div>

      <div class="grid grid-cols-7 gap-y-0.5">
        <button
          v-for="cell in cells"
          :key="cell.key"
          type="button"
          class="grid h-8 place-items-center rounded-md text-xs transition-colors hover:bg-gray-100 dark:hover:bg-gray-700"
          :class="[
            cell.key === draftDate
              ? 'bg-blue-500 font-semibold text-white hover:bg-blue-600 dark:hover:bg-blue-600'
              : 'text-gray-700 dark:text-gray-300',
            cell.outside && cell.key !== draftDate ? 'text-gray-300 dark:text-gray-600' : '',
          ]"
          @click="pickDay(cell.key)"
        >
          {{ cell.day }}
        </button>
      </div>

      <div
        class="mt-3 flex items-center justify-center gap-2 border-t border-gray-100 pt-3 dark:border-gray-700"
      >
        <label class="text-[10px] font-medium uppercase tracking-wide text-gray-400">
          Hodina
          <select
            v-model.number="draftHour"
            class="mt-1 block rounded-md border border-gray-300 bg-white px-2 py-1 text-sm text-gray-700 dark:border-gray-600 dark:bg-gray-900 dark:text-gray-200"
          >
            <option v-for="hour in 24" :key="hour - 1" :value="hour - 1">
              {{ pad(hour - 1) }}
            </option>
          </select>
        </label>
        <span class="mt-4 text-lg text-gray-400">:</span>
        <label class="text-[10px] font-medium uppercase tracking-wide text-gray-400">
          Minuta
          <select
            v-model.number="draftMinute"
            class="mt-1 block rounded-md border border-gray-300 bg-white px-2 py-1 text-sm text-gray-700 dark:border-gray-600 dark:bg-gray-900 dark:text-gray-200"
          >
            <option v-for="minute in 60" :key="minute - 1" :value="minute - 1">
              {{ pad(minute - 1) }}
            </option>
          </select>
        </label>
      </div>

      <div class="mt-3 flex items-center gap-2">
        <button
          type="button"
          class="rounded-md px-2 py-1 text-xs text-gray-500 hover:bg-gray-100 dark:text-gray-400 dark:hover:bg-gray-700"
          @click="clear"
        >
          Vymazat
        </button>
        <button
          type="button"
          class="rounded-md px-2 py-1 text-xs text-blue-600 hover:bg-blue-50 dark:text-blue-400 dark:hover:bg-blue-500/10"
          @click="pickNow"
        >
          Nyní
        </button>
        <button
          type="button"
          class="ml-auto rounded-md bg-blue-500 px-3 py-1 text-xs font-medium text-white hover:bg-blue-600"
          @click="apply"
        >
          Hotovo
        </button>
      </div>
    </div>
  </div>
</template>
