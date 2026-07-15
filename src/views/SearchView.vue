<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import BaseCheckbox from '@/components/BaseCheckbox.vue';
import QueryBuilder from '@/components/QueryBuilder.vue';
import RawFileViewer from '@/components/RawFileViewer.vue';
import SearchChart from '@/components/SearchChart.vue';
import Spinner from '@/components/Spinner.vue';
import { formatLogTime } from '@/composables/useTimeMode';
import { useAppStore } from '@/stores/appStore';
import { buildHighlighter, slotClass, type HighlightPart } from '@/utils/regexQuery';
import type { SavedQuery, SearchHit, SearchMeta, SearchProgress } from '@/types';

const PAGE_SIZE = 500;

const store = useAppStore();
const query = ref('');
const isRegex = ref(false);
const caseSensitive = ref(false);
const contextLines = ref(2);
const searching = ref(false);
const progress = ref<SearchProgress | null>(null);
const loadingMore = ref(false);
const error = ref<string | null>(null);
const meta = ref<SearchMeta | null>(null);
const hits = ref<SearchHit[]>([]);
const expanded = ref<Set<number>>(new Set());
const presets = ref<SavedQuery[]>([]);

const serviceNames = computed(() =>
  Object.fromEntries(store.config.services.map((s) => [s.id, s.name])),
);

// Per hit, not per table: a merged result may span services on different log clocks,
// and each row then has to say which one it is read on. Recomputes on the toggle.
const times = computed(() =>
  hits.value.map((hit) => formatLogTime(hit.timestamp, store.logOffset(hit.serviceId))),
);

const allQueries = computed(() => [...presets.value, ...store.config.savedQueries]);

function applyQuery(saved: SavedQuery) {
  // The builder owns the query field while open; a saved query is a plain one
  showBuilder.value = false;
  query.value = saved.query;
  isRegex.value = saved.isRegex;
}

// --- Query builder ---

const BUILDER_OPEN_KEY = 'loglooker.queryBuilderOpen';
const showBuilder = ref(localStorage.getItem(BUILDER_OPEN_KEY) === '1');

watch(showBuilder, (open) => {
  localStorage.setItem(BUILDER_OPEN_KEY, open ? '1' : '0');
});

// The builder compiles its criteria to a plain regex; closing it leaves the
// compiled pattern in the field, free to be edited by hand
function onCompiled(compiled: string) {
  query.value = compiled;
  isRegex.value = true;
}

async function saveCurrentQuery() {
  const name = prompt('Name for this query:', query.value.slice(0, 40));
  if (!name) return;
  store.config.savedQueries.push({ name, query: query.value, isRegex: isRegex.value });
  try {
    await store.saveConfig();
  } catch (e) {
    error.value = String(e);
  }
}

async function search() {
  if (store.selectedServices.length === 0 || searching.value) return;
  searching.value = true;
  progress.value = null;
  error.value = null;
  // The previous result stays on screen (blurred) until the new one arrives
  const submittedQuery = query.value;
  const submittedIsRegex = isRegex.value;
  const submittedCaseSensitive = caseSensitive.value;
  try {
    const result = await invoke<SearchMeta>('search_logs', {
      request: {
        serviceIds: store.selectedServices.map((s) => s.id),
        dateFrom: store.dateFrom,
        dateTo: store.dateTo,
        query: submittedQuery,
        isRegex: submittedIsRegex,
        caseSensitive: submittedCaseSensitive,
        contextLines: contextLines.value,
      },
    });
    meta.value = result;
    hits.value = [];
    expanded.value = new Set();
    sortField.value = 'time';
    sortAsc.value = true;
    searchedQuery.value = submittedQuery;
    searchedIsRegex.value = submittedIsRegex;
    searchedCaseSensitive.value = submittedCaseSensitive;
    await loadMore();
  } catch (e) {
    error.value = String(e);
  } finally {
    searching.value = false;
  }
}

async function loadMore() {
  if (!meta.value || loadingMore.value || hits.value.length >= meta.value.totalHits) return;
  loadingMore.value = true;
  try {
    const page = await invoke<SearchHit[]>('get_search_hits', {
      offset: hits.value.length,
      limit: PAGE_SIZE,
    });
    hits.value.push(...page);
  } catch (e) {
    error.value = String(e);
  } finally {
    loadingMore.value = false;
  }
}

function onResultsScroll(event: Event) {
  // While a new search runs, the visible hits belong to the previous result;
  // loading more would mix pages of the new backend state into it
  if (searching.value) return;
  const el = event.target as HTMLElement;
  if (el.scrollTop + el.clientHeight >= el.scrollHeight - 400) {
    loadMore();
  }
}

type SortField = 'time' | 'service' | 'operation' | 'file';

interface ColumnDef {
  key: 'time' | 'service' | 'fields' | 'line';
  label: string;
  sortField: SortField;
  width: string;
}

const ALL_COLUMNS: ColumnDef[] = [
  { key: 'time', label: 'Time', sortField: 'time', width: 'w-40' },
  { key: 'service', label: 'Service', sortField: 'service', width: 'w-28' },
  { key: 'fields', label: 'Operation / Login', sortField: 'operation', width: 'w-44' },
  { key: 'line', label: 'Line', sortField: 'file', width: '' },
];

const COLUMNS_STORAGE_KEY = 'loglooker.searchColumns';

function loadColumnPrefs(): { order: string[]; hidden: string[] } {
  try {
    const saved = JSON.parse(localStorage.getItem(COLUMNS_STORAGE_KEY) ?? '');
    if (Array.isArray(saved.order) && Array.isArray(saved.hidden)) {
      const valid = ALL_COLUMNS.map((c) => c.key as string);
      const order = saved.order.filter((k: string) => valid.includes(k));
      valid.forEach((k) => {
        if (!order.includes(k)) order.push(k);
      });
      return { order, hidden: saved.hidden.filter((k: string) => valid.includes(k)) };
    }
  } catch {
    // fall through to defaults
  }
  return { order: ALL_COLUMNS.map((c) => c.key), hidden: [] };
}

const columnPrefs = ref(loadColumnPrefs());
const showColumnMenu = ref(false);
const dragKey = ref<string | null>(null);

function saveColumnPrefs() {
  localStorage.setItem(COLUMNS_STORAGE_KEY, JSON.stringify(columnPrefs.value));
}

const visibleColumns = computed(() =>
  columnPrefs.value.order
    .filter((key) => !columnPrefs.value.hidden.includes(key))
    .map((key) => ALL_COLUMNS.find((c) => c.key === key))
    .filter((c): c is ColumnDef => !!c),
);

function toggleColumn(key: string) {
  const hidden = columnPrefs.value.hidden;
  const index = hidden.indexOf(key);
  if (index >= 0) {
    hidden.splice(index, 1);
  } else if (visibleColumns.value.length > 1) {
    hidden.push(key);
  }
  saveColumnPrefs();
}

function onColumnDrop(targetKey: string) {
  if (!dragKey.value || dragKey.value === targetKey) return;
  const order = columnPrefs.value.order;
  const from = order.indexOf(dragKey.value);
  const to = order.indexOf(targetKey);
  if (from < 0 || to < 0) return;
  order.splice(to, 0, ...order.splice(from, 1));
  dragKey.value = null;
  saveColumnPrefs();
}

const sortField = ref<SortField>('time');
const sortAsc = ref(true);

async function sortBy(column: ColumnDef) {
  if (!meta.value || searching.value) return;
  if (sortField.value === column.sortField) {
    sortAsc.value = !sortAsc.value;
  } else {
    sortField.value = column.sortField;
    sortAsc.value = true;
  }
  error.value = null;
  try {
    await invoke('sort_search_hits', { field: sortField.value, ascending: sortAsc.value });
    expanded.value = new Set();
    hits.value = [];
    await loadMore();
  } catch (e) {
    error.value = String(e);
  }
}

function toggleExpanded(index: number) {
  if (expanded.value.has(index)) {
    expanded.value.delete(index);
  } else {
    expanded.value.add(index);
  }
  expanded.value = new Set(expanded.value);
}

const rawView = ref<SearchHit | null>(null);
// The table is the chart's accessible twin: every charted value is readable here
const tab = ref<'table' | 'chart'>('table');
// The query the current result came from — the chart labels itself with it, and
// it must not follow the input box while the user types the next search.
// The flags travel with it into the raw viewer, which pre-applies the search.
const searchedQuery = ref('');
const searchedIsRegex = ref(false);
const searchedCaseSensitive = ref(false);

// --- Inline highlighting, coloured per capture group (slots shared with charts) ---

const highlighter = computed(() =>
  buildHighlighter(searchedQuery.value, searchedIsRegex.value, searchedCaseSensitive.value),
);

// Parts are cached per hit object: rows re-render on expand and scroll far
// more often than the searched query changes
let partsCache = new WeakMap<SearchHit, HighlightPart[]>();
watch(highlighter, () => {
  partsCache = new WeakMap();
});

function lineParts(hit: SearchHit): HighlightPart[] {
  const active = highlighter.value;
  if (!active) return [{ text: hit.line, slot: null }];
  let parts = partsCache.get(hit);
  if (!parts) {
    parts = active.parts(hit.line);
    partsCache.set(hit, parts);
  }
  return parts;
}

// Colour legend for the searched query's capture groups, shown above the results
const groupChips = computed(() =>
  (meta.value?.groupNames ?? []).map((name, i) => ({ name, slot: (i % 8) + 1 })),
);

const progressText = computed(() => {
  const p = progress.value;
  if (!p) return 'Starting search...';
  if (p.phase === 'sorting') return `Sorting ${p.hits.toLocaleString()} hits...`;
  if (p.filesTotal === 0) return 'No cached files in range...';
  return (
    `Scanning files ${p.filesDone}/${p.filesTotal} — ` +
    `${p.hits.toLocaleString()} hits, ${p.linesScanned.toLocaleString()} lines`
  );
});

let unlistenProgress: UnlistenFn | null = null;

onMounted(async () => {
  unlistenProgress = await listen<SearchProgress>('search-progress', (event) => {
    progress.value = event.payload;
  });
  try {
    if (store.config.services.length === 0) await store.loadConfig();
    presets.value = await invoke<SavedQuery[]>('get_preset_queries');
  } catch (e) {
    error.value = String(e);
  }
});

onUnmounted(() => {
  unlistenProgress?.();
});
</script>

<template>
  <div>
    <div
      v-if="error"
      class="mb-4 px-4 py-2 rounded-md bg-red-50 dark:bg-red-900/30 border border-red-200 dark:border-red-800 text-sm text-red-700 dark:text-red-400 flex items-center justify-between"
    >
      <span class="break-all">{{ error }}</span>
      <button
        class="ml-4 shrink-0 hover:underline"
        @click="error = null"
      >
        Dismiss
      </button>
    </div>

    <div class="flex items-center gap-2 mb-2">
      <input
        v-model="query"
        type="text"
        spellcheck="false"
        :readonly="showBuilder"
        :placeholder="
          showBuilder
            ? 'Compiled from the builder below...'
            : 'Search cached logs (substring or regex, empty = all lines)...'
        "
        class="flex-1 px-3 py-1.5 text-sm font-mono rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800"
        :class="showBuilder ? 'text-gray-500 dark:text-gray-400' : ''"
        :title="showBuilder ? 'The builder owns the query while it is open' : ''"
        @keydown.enter="search"
      >
      <button
        class="px-4 py-1.5 text-xs font-semibold rounded-md bg-blue-500 text-white hover:bg-blue-600 disabled:opacity-50 transition-colors"
        :disabled="searching || store.selectedServices.length === 0"
        @click="search"
      >
        {{ searching ? 'Searching...' : 'Search' }}
      </button>
    </div>

    <div class="flex items-center gap-4 mb-4 text-xs text-gray-600 dark:text-gray-400">
      <BaseCheckbox
        v-model="isRegex"
        :disabled="showBuilder"
      >
        Regex
      </BaseCheckbox>
      <BaseCheckbox v-model="caseSensitive">
        Case sensitive
      </BaseCheckbox>
      <button
        class="px-2 py-1 border rounded"
        :class="
          showBuilder
            ? 'border-blue-400 dark:border-blue-600 bg-blue-100 dark:bg-blue-900/40 text-blue-700 dark:text-blue-300'
            : 'border-gray-300 dark:border-gray-600 hover:text-gray-800 dark:hover:text-gray-200'
        "
        title="Build the query from multiple criteria; each one gets a colour in the results and charts"
        @click="showBuilder = !showBuilder"
      >
        Builder
      </button>
      <label class="flex items-center gap-1.5">
        Context
        <select
          v-model.number="contextLines"
          class="px-1 py-0.5 rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800"
        >
          <option :value="0">0</option>
          <option :value="2">2</option>
          <option :value="5">5</option>
          <option :value="10">10</option>
        </select>
      </label>

      <select
        class="px-2 py-1 rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800"
        @change="
          (e) => {
            const saved = allQueries[Number((e.target as HTMLSelectElement).value)];
            if (saved) applyQuery(saved);
            (e.target as HTMLSelectElement).value = '';
          }
        "
      >
        <option value="">
          Saved queries...
        </option>
        <option
          v-for="(saved, i) in allQueries"
          :key="i"
          :value="i"
        >
          {{ saved.name }}
        </option>
      </select>
      <button
        class="px-2 py-1 border border-gray-300 dark:border-gray-600 rounded hover:text-gray-800 dark:hover:text-gray-200 disabled:opacity-50"
        :disabled="!query"
        @click="saveCurrentQuery"
      >
        Save query
      </button>

      <span class="ml-auto">
        {{ store.selectedServices.length }} services selected ({{ store.environment }}),
        {{ store.dateFrom }} → {{ store.dateTo }}
        <router-link
          to="/"
          class="text-blue-500 hover:underline"
        >change</router-link>
      </span>
    </div>

    <QueryBuilder
      v-if="showBuilder"
      @compiled="onCompiled"
      @search="search"
    />

    <div
      v-if="meta"
      class="mb-2 text-xs text-gray-500 dark:text-gray-400 flex items-center gap-3"
      :class="searching ? 'opacity-50' : ''"
    >
      <span>
        {{ meta.totalHits.toLocaleString() }} hits — {{ meta.filesScanned }} files,
        {{ meta.linesScanned.toLocaleString() }} lines in {{ meta.durationMs }} ms
      </span>
      <span
        v-for="chip in groupChips"
        :key="chip.name"
        class="flex items-center gap-1.5 text-gray-600 dark:text-gray-300"
      >
        <span
          class="w-2.5 h-2.5 rounded-sm shrink-0"
          :style="{ backgroundColor: `var(--chart-${chip.slot})` }"
        />
        <span class="truncate max-w-[10rem]">{{ chip.name }}</span>
      </span>
      <span v-if="tab === 'table' && hits.length < meta.totalHits">
        showing {{ hits.length.toLocaleString() }} (scroll to load more)
      </span>

      <div class="ml-auto flex items-center rounded-md border border-gray-300 dark:border-gray-600 overflow-hidden">
        <button
          v-for="option in (['table', 'chart'] as const)"
          :key="option"
          class="px-3 py-1 capitalize"
          :class="
            tab === option
              ? 'bg-blue-500 text-white'
              : 'hover:text-gray-800 dark:hover:text-gray-200'
          "
          @click="tab = option"
        >
          {{ option }}
        </button>
      </div>

      <div
        v-if="tab === 'table'"
        class="relative"
      >
        <button
          class="px-2 py-1 border border-gray-300 dark:border-gray-600 rounded hover:text-gray-800 dark:hover:text-gray-200"
          @click="showColumnMenu = !showColumnMenu"
        >
          Columns
        </button>
        <div
          v-if="showColumnMenu"
          class="absolute right-0 mt-1 z-20 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-md shadow-lg p-1"
        >
          <BaseCheckbox
            v-for="col in ALL_COLUMNS"
            :key="col.key"
            :model-value="!columnPrefs.hidden.includes(col.key)"
            class="flex px-2 py-1 whitespace-nowrap hover:bg-gray-50 dark:hover:bg-gray-700 rounded"
            @update:model-value="toggleColumn(col.key)"
          >
            {{ col.label }}
          </BaseCheckbox>
        </div>
      </div>
    </div>

    <div
      v-if="meta"
      class="relative"
    >
      <SearchChart
        v-if="tab === 'chart'"
        :query="searchedQuery"
      />

      <div
        v-if="tab === 'table'"
        class="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden"
      >
        <div
          class="max-h-[calc(100vh-260px)] overflow-auto"
          @scroll.passive="onResultsScroll"
        >
          <table class="w-full min-w-[48rem] text-xs font-mono">
            <thead>
              <tr>
                <th
                  v-for="col in visibleColumns"
                  :key="col.key"
                  draggable="true"
                  class="sticky top-0 z-10 bg-white dark:bg-gray-800 text-left py-2 px-3 font-medium text-gray-500 dark:text-gray-400 border-b border-gray-200 dark:border-gray-700 cursor-pointer select-none"
                  :class="[col.width, dragKey === col.key ? 'opacity-50' : '']"
                  :title="`Sort by ${col.label}; drag to reorder`"
                  @dragstart="dragKey = col.key"
                  @dragend="dragKey = null"
                  @dragover.prevent
                  @drop="onColumnDrop(col.key)"
                  @click="sortBy(col)"
                >
                  {{ col.label }}
                  <span v-if="sortField === col.sortField">{{ sortAsc ? '↑' : '↓' }}</span>
                </th>
              </tr>
            </thead>
            <tbody>
              <template
                v-for="(hit, i) in hits"
                :key="i"
              >
                <tr
                  class="border-b border-gray-100 dark:border-gray-800 hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer align-top"
                  @click="toggleExpanded(i)"
                >
                  <td
                    v-for="col in visibleColumns"
                    :key="col.key"
                    class="py-1.5 px-3"
                    :class="{
                      'text-gray-500 dark:text-gray-400 whitespace-nowrap': col.key === 'time',
                      'text-gray-500 dark:text-gray-400': col.key === 'fields',
                      'text-gray-700 dark:text-gray-300 break-all': col.key === 'line',
                    }"
                  >
                    <template v-if="col.key === 'time'">
                      {{ times[i].text }}
                      <span
                        v-if="times[i].zone"
                        class="text-gray-400 dark:text-gray-500"
                      >({{ times[i].zone }})</span>
                    </template>
                    <template v-else-if="col.key === 'service'">
                      <span
                        class="inline-block px-1.5 py-0.5 text-[10px] font-semibold rounded"
                        :class="
                          hit.serviceId.startsWith('production/')
                            ? 'bg-red-100 dark:bg-red-900/30 text-red-700 dark:text-red-400'
                            : 'bg-blue-100 dark:bg-blue-900/30 text-blue-700 dark:text-blue-400'
                        "
                      >
                        {{ serviceNames[hit.serviceId] ?? hit.serviceId }}
                      </span>
                    </template>
                    <template v-else-if="col.key === 'fields'">
                      <div
                        v-if="hit.fields.operation"
                        class="truncate"
                        :title="hit.fields.operation"
                      >
                        {{ hit.fields.operation }}
                      </div>
                      <div
                        v-if="hit.fields.idLogin"
                        class="truncate text-[10px] text-gray-400"
                        :title="hit.fields.idLogin"
                      >
                        {{ hit.fields.idLogin }}
                      </div>
                    </template>
                    <template v-else>
                      <template
                        v-for="(part, p) in lineParts(hit)"
                        :key="p"
                      >
                        <span
                          v-if="part.slot !== null"
                          :class="slotClass(part.slot)"
                        >{{ part.text }}</span><template v-else>
                          {{ part.text }}
                        </template>
                      </template>
                      <button
                        class="text-gray-400 text-[10px] whitespace-nowrap hover:text-blue-500 hover:underline"
                        title="Open the raw file at this line"
                        @click.stop="rawView = hit"
                      >
                        — {{ hit.file }}:{{ hit.lineNumber }}
                      </button>
                    </template>
                  </td>
                </tr>
                <tr
                  v-if="expanded.has(i)"
                  class="border-b border-gray-200 dark:border-gray-700"
                >
                  <td
                    :colspan="visibleColumns.length"
                    class="px-3 py-2 bg-gray-50 dark:bg-gray-900/50"
                  >
                    <pre
                      class="whitespace-pre-wrap break-all text-[11px] leading-relaxed"
                    ><span class="text-gray-400">{{ hit.contextBefore.join('\n') }}</span>
  <span class="text-gray-900 dark:text-gray-100 font-semibold"><template
    v-for="(part, p) in lineParts(hit)"
    :key="p"
  ><span
    v-if="part.slot !== null"
    :class="slotClass(part.slot)"
  >{{ part.text }}</span><template v-else>{{ part.text }}</template></template></span>
  <span class="text-gray-400">{{ hit.contextAfter.join('\n') }}</span></pre>
                    <button
                      class="mt-2 px-2 py-1 text-[10px] border border-gray-300 dark:border-gray-600 rounded text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200"
                      @click.stop="rawView = hit"
                    >
                      Open raw file at line {{ hit.lineNumber.toLocaleString() }}
                    </button>
                  </td>
                </tr>
              </template>
              <tr v-if="loadingMore">
                <td
                  :colspan="visibleColumns.length"
                  class="py-3 text-center text-gray-400"
                >
                  <span class="inline-flex items-center gap-2">
                    <Spinner size="sm" />
                    Loading more ({{ hits.length.toLocaleString() }}/{{ meta.totalHits.toLocaleString() }})...
                  </span>
                </td>
              </tr>
              <tr v-if="meta.totalHits === 0">
                <td
                  :colspan="visibleColumns.length"
                  class="py-8 text-center text-gray-400"
                >
                  No matches.
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>

      <div
        v-if="searching"
        class="absolute inset-0 z-20 flex items-center justify-center rounded-lg backdrop-blur-[2px] bg-white/40 dark:bg-gray-900/40"
      >
        <div
          class="flex flex-col items-center gap-2 min-w-[22rem] px-6 py-4 rounded-lg bg-white/95 dark:bg-gray-800/95 border border-gray-200 dark:border-gray-700 shadow-lg"
        >
          <Spinner />
          <div class="text-xs text-gray-700 dark:text-gray-200">
            {{ progressText }}
          </div>
          <div
            v-if="progress?.currentFile"
            class="max-w-[24rem] truncate text-[10px] font-mono text-gray-400"
            :title="progress.currentFile"
          >
            {{ progress.currentFile }}
          </div>
        </div>
      </div>
    </div>

    <div
      v-else-if="searching"
      class="py-16 flex flex-col items-center gap-3 text-gray-400 text-sm"
    >
      <Spinner />
      <div>{{ progressText }}</div>
      <div
        v-if="progress?.currentFile"
        class="text-xs font-mono"
      >
        {{ progress.currentFile }}
      </div>
    </div>

    <div
      v-else
      class="py-16 text-center text-gray-400 text-sm"
    >
      Select services and a date range on the Services page, sync, then search here.
    </div>

    <RawFileViewer
      v-if="rawView"
      :key="`${rawView.serviceId}:${rawView.file}:${rawView.lineNumber}`"
      :service-id="rawView.serviceId"
      :service-name="serviceNames[rawView.serviceId] ?? rawView.serviceId"
      :file="rawView.file"
      :line="rawView.lineNumber"
      :initial-query="searchedQuery"
      :initial-is-regex="searchedIsRegex"
      :initial-case-sensitive="searchedCaseSensitive"
      @close="rawView = null"
    />
  </div>
</template>
