<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { UnlistenFn } from '@tauri-apps/api/event';
import BaseCheckbox from '@/components/BaseCheckbox.vue';
import BaseSelect, { type SelectOption } from '@/components/BaseSelect.vue';
import ChartCanvas from '@/components/ChartCanvas.vue';
import { displayZone, shiftLogLabel } from '@/composables/useTimeMode';
import { useAppStore } from '@/stores/appStore';
import type { ChartData, ChartRequest } from '@/types';

const SETTINGS_KEY = 'loglooker.chartSettings';
const DEBOUNCE_MS = 250;
// Never cycle categorical hues: a timeline may show at most eight series, and
// what does not fit is reported as "not shown" rather than folded into a colour
// that already means something else. A category chart is one series, so its
// bars are all slot 1 and it can show many more.
const TIMELINE_TOP_N = [3, 5, 8];
const CATEGORY_TOP_N = [5, 10, 15, 20, 30];

const props = defineProps<{
  query: string;
  // A chart in its own window has no "open in window" button and cannot navigate
  windowed?: boolean;
  initial?: ChartRequest;
}>();

interface ChartSettings extends ChartRequest {
  type: 'bar' | 'line';
  stacked: boolean;
}

const DEFAULTS: ChartSettings = {
  mode: 'timeline',
  bucket: 'auto',
  groupBy: 'none',
  customRegex: null,
  metric: 'count',
  valueSource: 'field',
  groupField: null,
  valueField: null,
  topN: 5,
  type: 'bar',
  stacked: true,
};

function loadSettings(): ChartSettings {
  if (props.initial) return { ...DEFAULTS, ...props.initial };
  try {
    const saved = JSON.parse(localStorage.getItem(SETTINGS_KEY) ?? '');
    return { ...DEFAULTS, ...saved };
  } catch {
    return { ...DEFAULTS };
  }
}

const settings = ref<ChartSettings>(loadSettings());
const data = ref<ChartData | null>(null);
const error = ref<string | null>(null);
const loading = ref(false);

const needsRegex = computed(
  () =>
    settings.value.groupBy === 'custom' ||
    (settings.value.metric !== 'count' && settings.value.valueSource === 'custom'),
);

// What can be grouped or measured comes from the loaded plugins, not from the
// app: a plugin for a different log shape brings its own dimensions.
const store = useAppStore();
const groupFields = computed(() => store.fields);
const valueFields = computed(() => store.fields.filter((field) => field.type === 'number'));
const isCategory = computed(() => settings.value.mode === 'category');
const topNOptions = computed(() => (isCategory.value ? CATEGORY_TOP_N : TIMELINE_TOP_N));

function request(): ChartRequest {
  const s = settings.value;
  return {
    mode: s.mode,
    bucket: s.bucket,
    groupBy: s.groupBy,
    customRegex: needsRegex.value ? s.customRegex : null,
    metric: s.metric,
    valueSource: s.valueSource,
    groupField: s.groupBy === 'field' ? s.groupField : null,
    valueField: s.valueSource === 'field' ? s.valueField : null,
    topN: s.topN,
  };
}

// A category chart of nothing is a single bar - the one thing a bar chart must
// never be. Grouping is what gives it its bars, so pick one on the way in:
// the first plugin-declared field if there is one, else the service.
watch(
  () => settings.value.mode,
  (mode) => {
    if (mode === 'category' && settings.value.groupBy === 'none') {
      const first = groupFields.value[0];
      groupChoice.value = first ? `field:${first.key}` : 'service';
    }
  },
);

// The group-by and value dropdowns each hold one string, with a field written
// as `field:<key>` - the same form a plugin's chart presets use. Keeping it to
// one control per dimension avoids a second dropdown that only sometimes exists.
const groupChoice = computed({
  get: () =>
    settings.value.groupBy === 'field'
      ? `field:${settings.value.groupField ?? ''}`
      : settings.value.groupBy,
  set: (choice: string) => {
    const key = choice.startsWith('field:') ? choice.slice('field:'.length) : null;
    settings.value.groupBy = key ? 'field' : (choice as ChartRequest['groupBy']);
    settings.value.groupField = key;
  },
});

const valueChoice = computed({
  get: () =>
    settings.value.valueSource === 'custom' ? 'custom' : `field:${settings.value.valueField ?? ''}`,
  set: (choice: string) => {
    const key = choice.startsWith('field:') ? choice.slice('field:'.length) : null;
    settings.value.valueSource = key ? 'field' : 'custom';
    settings.value.valueField = key;
  },
});

// A field the loaded plugins no longer declare cannot be charted; fall back to
// the first that fits so a saved chart never sends an unresolvable request
watch(
  [() => settings.value.groupBy, groupFields],
  () => {
    if (settings.value.groupBy !== 'field') return;
    if (!groupFields.value.some((f) => f.key === settings.value.groupField)) {
      settings.value.groupField = groupFields.value[0]?.key ?? null;
    }
  },
  { immediate: true },
);
watch(
  [() => settings.value.valueSource, valueFields],
  () => {
    if (settings.value.valueSource !== 'field') return;
    if (!valueFields.value.some((f) => f.key === settings.value.valueField)) {
      settings.value.valueField = valueFields.value[0]?.key ?? null;
    }
  },
  { immediate: true },
);

let timer: ReturnType<typeof setTimeout> | undefined;
let token = 0;

async function aggregate() {
  const current = ++token;
  loading.value = true;
  error.value = null;
  try {
    const result = await invoke<ChartData>('aggregate_search_hits', { request: request() });
    if (current !== token) return;
    data.value = result;
  } catch (e) {
    if (current !== token) return;
    error.value = String(e);
    data.value = null;
  } finally {
    if (current === token) loading.value = false;
  }
}

watch(
  settings,
  () => {
    if (!props.windowed) localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings.value));
    // Top N lists differ per mode; keep the value on the list of the active one
    if (!topNOptions.value.includes(settings.value.topN)) {
      settings.value.topN = isCategory.value ? 10 : 5;
      return;
    }
    if (needsRegex.value && !settings.value.customRegex) return;
    clearTimeout(timer);
    timer = setTimeout(aggregate, DEBOUNCE_MS);
  },
  { deep: true },
);

let unlisten: UnlistenFn | null = null;

onMounted(async () => {
  await aggregate();
  // Chart windows share the backend's search state with the main window, so a
  // new search there must redraw them rather than leave a stale chart up.
  unlisten = await listen('search-updated', () => aggregate());
});

onUnmounted(() => {
  clearTimeout(timer);
  unlisten?.();
});

async function openInWindow() {
  try {
    await invoke('open_chart_window', { query: props.query, request: request() });
  } catch (e) {
    error.value = String(e);
  }
}

// --- Presentation ---

const chartType = computed<'bar' | 'line'>(() => (isCategory.value ? 'bar' : settings.value.type));
const canStack = computed(
  () => !isCategory.value && settings.value.type === 'bar' && settings.value.groupBy !== 'none',
);
const showLegend = computed(() => (data.value?.series.length ?? 0) > 1);

// Buckets are cut from the naive log timestamps, so the axis reads on the log's clock
// until it is shifted onto the displayed one. Day buckets are exempt: they are cut on
// the log clock's midnight, and re-dating them in another zone would label a bucket by
// a boundary it does not have.
const axisZone = computed(() => {
  const seconds = data.value?.bucketSeconds ?? 0;
  if (isCategory.value || seconds >= 86400) return null;
  return displayZone(data.value?.logOffsetMinutes ?? null);
});

function displayLabel(label: string): string {
  const seconds = data.value?.bucketSeconds ?? 0;
  if (seconds >= 86400) return label;
  return shiftLogLabel(label, data.value?.logOffsetMinutes ?? null);
}

function formatX(label: string): string {
  if (isCategory.value) return label;
  const [date, time] = displayLabel(label).split('T');
  if (!time) return label;
  const seconds = data.value?.bucketSeconds ?? 0;
  if (seconds >= 86400) return date.slice(5).split('-').reverse().join('.');
  return seconds < 60 ? time : time.slice(0, 5);
}

function formatXLong(label: string): string {
  if (isCategory.value) return label;
  return displayLabel(label).replace('T', ' ');
}

const skipped = computed(() => {
  const d = data.value;
  if (!d) return [];
  const notes: string[] = [];
  if (d.skippedNoTime > 0) notes.push(`${d.skippedNoTime.toLocaleString()} without a timestamp`);
  if (d.skippedNoValue > 0) notes.push(`${d.skippedNoValue.toLocaleString()} without a value`);
  if (d.skippedNoKey > 0) notes.push(`${d.skippedNoKey.toLocaleString()} without a group`);
  return notes;
});

const MODE_OPTIONS: SelectOption<ChartSettings['mode']>[] = [
  { value: 'timeline', label: 'Time' },
  { value: 'category', label: 'Category' },
];

const BUCKET_OPTIONS: SelectOption<ChartSettings['bucket']>[] = [
  { value: 'auto', label: 'Auto' },
  { value: 'second', label: '1 s' },
  { value: 'minute', label: '1 min' },
  { value: 'fiveMinutes', label: '5 min' },
  { value: 'fifteenMinutes', label: '15 min' },
  { value: 'hour', label: '1 hour' },
  { value: 'sixHours', label: '6 hours' },
  { value: 'day', label: '1 day' },
];

const METRIC_OPTIONS: SelectOption<ChartSettings['metric']>[] = [
  { value: 'count', label: 'Count' },
  { value: 'sum', label: 'Sum' },
  { value: 'avg', label: 'Average' },
  { value: 'p50', label: 'p50' },
  { value: 'p95', label: 'p95' },
  { value: 'p99', label: 'p99' },
  { value: 'min', label: 'Min' },
  { value: 'max', label: 'Max' },
];

const TYPE_OPTIONS: SelectOption<ChartSettings['type']>[] = [
  { value: 'bar', label: 'Bar' },
  { value: 'line', label: 'Line' },
];

// Whatever the loaded plugins extract is offered alongside the built-in dimensions.
const groupOptions = computed<SelectOption<string>[]>(() => [
  ...(isCategory.value ? [] : [{ value: 'none', label: 'Nothing' }]),
  { value: 'service', label: 'Service' },
  { value: 'file', label: 'File' },
  { value: 'custom', label: 'Custom regex' },
  { value: 'matchedGroup', label: 'Matched group' },
  ...groupFields.value.map((field) => ({ value: `field:${field.key}`, label: field.label })),
]);

// Only number fields can be aggregated; a plugin says which are.
const valueOptions = computed<SelectOption<string>[]>(() => [
  ...valueFields.value.map((field) => ({ value: `field:${field.key}`, label: field.label })),
  { value: 'custom', label: 'Custom regex value' },
]);

const topNSelectOptions = computed<SelectOption<number>[]>(() =>
  topNOptions.value.map((n) => ({ value: n, label: String(n) })),
);
</script>

<template>
  <div>
    <div
      class="flex flex-wrap items-center gap-x-4 gap-y-2 mb-3 text-xs text-gray-600 dark:text-gray-400"
    >
      <label class="flex items-center gap-1.5">
        X axis
        <BaseSelect
          v-model="settings.mode"
          :options="MODE_OPTIONS"
        />
      </label>

      <label
        v-if="!isCategory"
        class="flex items-center gap-1.5"
      >
        Bucket
        <BaseSelect
          v-model="settings.bucket"
          :options="BUCKET_OPTIONS"
        />
      </label>

      <label class="flex items-center gap-1.5">
        Group by
        <BaseSelect
          v-model="groupChoice"
          :options="groupOptions"
        />
      </label>

      <label class="flex items-center gap-1.5">
        Metric
        <BaseSelect
          v-model="settings.metric"
          :options="METRIC_OPTIONS"
        />
      </label>

      <label
        v-if="settings.metric !== 'count'"
        class="flex items-center gap-1.5"
      >
        of
        <BaseSelect
          v-model="valueChoice"
          :options="valueOptions"
        />
      </label>

      <label
        v-if="settings.groupBy !== 'matchedGroup'"
        class="flex items-center gap-1.5"
      >
        Top
        <BaseSelect
          v-model="settings.topN"
          :options="topNSelectOptions"
        />
      </label>

      <template v-if="!isCategory">
        <label class="flex items-center gap-1.5">
          Type
          <BaseSelect
            v-model="settings.type"
            :options="TYPE_OPTIONS"
          />
        </label>
        <BaseCheckbox
          v-if="canStack"
          v-model="settings.stacked"
        >
          Stacked
        </BaseCheckbox>
      </template>

      <button
        v-if="!windowed"
        class="ml-auto px-2 py-1 border border-gray-300 dark:border-gray-600 rounded hover:text-gray-800 dark:hover:text-gray-200"
        title="Open this chart in its own window"
        @click="openInWindow"
      >
        Open in window
      </button>
    </div>

    <div
      v-if="needsRegex"
      class="flex items-center gap-2 mb-3 text-xs"
    >
      <input
        v-model="settings.customRegex"
        type="text"
        spellcheck="false"
        placeholder="Handling (?<key>\w+) .* took (?<value>\d+) ms"
        class="flex-1 px-2 py-1 font-mono rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800"
      >
      <span class="text-gray-400 whitespace-nowrap">
        (?&lt;key&gt;...) = category, (?&lt;value&gt;...) = number
      </span>
    </div>

    <div
      v-if="error"
      class="mb-3 px-4 py-2 rounded-md bg-red-50 dark:bg-red-900/30 border border-red-200 dark:border-red-800 text-sm text-red-700 dark:text-red-400 break-all"
    >
      {{ error }}
    </div>

    <div
      class="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg p-4 transition-opacity"
      :class="loading ? 'opacity-60' : ''"
    >
      <div class="flex items-baseline gap-2 mb-3">
        <span class="text-sm font-medium text-gray-800 dark:text-gray-100">
          {{ data?.metricLabel ?? 'Hits' }}
        </span>
        <span
          v-if="data?.bucketLabel"
          class="text-xs text-gray-500 dark:text-gray-400"
        >
          per {{ data.bucketLabel }}
        </span>
        <span
          v-if="data && data.charted > 0"
          class="ml-auto text-xs text-gray-500 dark:text-gray-400 tabular-nums"
        >
          {{ data.charted.toLocaleString() }} hits charted
        </span>
      </div>

      <div
        v-if="showLegend"
        class="flex flex-wrap gap-x-4 gap-y-1 mb-3 text-xs"
      >
        <span
          v-for="(s, i) in data?.series"
          :key="s.name"
          class="flex items-center gap-1.5 text-gray-600 dark:text-gray-300"
        >
          <span
            class="w-2.5 h-2.5 rounded-sm shrink-0"
            :style="{ backgroundColor: `var(--chart-${(i % 8) + 1})` }"
          />
          <span class="truncate max-w-[16rem]">{{ s.name }}</span>
        </span>
      </div>

      <ChartCanvas
        v-if="data && data.labels.length > 0"
        :labels="data.labels"
        :series="data.series"
        :type="chartType"
        :stacked="canStack && settings.stacked"
        :horizontal="isCategory"
        :metric-label="data.metricLabel"
        :format-x="formatX"
        :format-x-long="formatXLong"
      />
      <div
        v-else-if="needsRegex && !settings.customRegex"
        class="py-16 text-center text-sm text-gray-400"
      >
        Enter a regex with a capture group to chart it.
      </div>
      <div
        v-else
        class="py-16 text-center text-sm text-gray-400"
      >
        Nothing to chart - the search has no hits that carry what this chart needs.
      </div>

      <div
        v-if="data && (skipped.length > 0 || data.otherGroups > 0 || axisZone)"
        class="mt-3 pt-3 border-t border-gray-100 dark:border-gray-700 text-[11px] text-gray-400 flex flex-wrap gap-x-3"
      >
        <span v-if="data.otherGroups > 0">
          {{ data.otherGroups.toLocaleString() }} more
          {{ data.otherGroups === 1 ? 'group' : 'groups' }} not shown.
        </span>
        <span v-if="skipped.length > 0"> Hits left out: {{ skipped.join(', ') }}. </span>
        <span
          v-if="axisZone"
          class="ml-auto"
        > Times in {{ axisZone }}. </span>
      </div>
    </div>
  </div>
</template>
