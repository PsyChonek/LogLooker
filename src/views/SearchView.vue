<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { save } from '@tauri-apps/plugin-dialog';
import BaseCheckbox from '@/components/BaseCheckbox.vue';
import BaseSelect, { type SelectOption } from '@/components/BaseSelect.vue';
import DataTable, { type DataTableColumn, type SortState } from '@/components/DataTable.vue';
import DateTimePicker from '@/components/DateTimePicker.vue';
import QueryBuilder from '@/components/QueryBuilder.vue';
import RawFileViewer from '@/components/RawFileViewer.vue';
import RegexGuide from '@/components/RegexGuide.vue';
import SearchChart from '@/components/SearchChart.vue';
import Spinner from '@/components/Spinner.vue';
import {
  convertPickerTime,
  formatLogTime,
  pickerTimeToUtc,
  useTimeMode,
} from '@/composables/useTimeMode';
import { useAppStore } from '@/stores/appStore';
import {
  buildExtractorParts,
  buildHighlighter,
  paletteSlot,
  parseQuery,
  partClass,
  partStyle,
  type HighlightPart,
  type QuerySet,
} from '@/utils/regexQuery';
import type {
  ExportRequest,
  ExportResult,
  SavedQuery,
  SearchHit,
  SearchMeta,
  SearchProgress,
} from '@/types';

const PAGE_SIZE = 500;
// A preview scans with a tiny hit cap: enough rows to judge the query, cheap
// enough to iterate on. The full search is one click away from the banner.
const PREVIEW_LIMIT = 100;
// The clipboard is not meant to carry a whole result; a file is. Past this the
// copy is cut and the UI reports by how much. A file export is never capped.
const CLIPBOARD_MAX_LINES = 50000;

const store = useAppStore();
const { mode: timeMode } = useTimeMode();

// Remembers the last filter (text plus its regex/case flags) across restarts,
// so the search view reopens on whatever the user last typed.
const FILTER_KEY = 'loglooker.searchFilter';

interface PersistedFilter {
  query?: string;
  isRegex?: boolean;
  caseSensitive?: boolean;
  timeEnabled?: boolean;
  timeFrom?: string;
  timeTo?: string;
}

function loadFilter(): PersistedFilter {
  try {
    return JSON.parse(localStorage.getItem(FILTER_KEY) ?? '{}') as PersistedFilter;
  } catch {
    return {};
  }
}

const savedFilter = loadFilter();
const query = ref(savedFilter.query ?? '');
const isRegex = ref(savedFilter.isRegex ?? true);
const caseSensitive = ref(savedFilter.caseSensitive ?? false);
const contextLines = ref(2);
const CONTEXT_OPTIONS: SelectOption<number>[] = [0, 2, 5, 10].map((n) => ({
  value: n,
  label: String(n),
}));

// Optional time window refining the day range. Values stay as wall-clock
// minutes in the currently displayed UTC/local mode while they are edited.
const timeEnabled = ref(savedFilter.timeEnabled ?? false);
const timeFrom = ref(savedFilter.timeFrom ?? '');
const timeTo = ref(savedFilter.timeTo ?? '');

// First enable seeds the window with the whole selected day range, so the
// inputs never open on an empty value the backend could not parse.
watch(timeEnabled, (on) => {
  if (!on) return;
  if (!timeFrom.value) timeFrom.value = `${store.dateFrom}T00:00`;
  if (!timeTo.value) timeTo.value = `${store.dateTo}T23:59`;
});

function wallTimeWindow(): { from: string; to: string } {
  const rangeFrom = `${store.dateFrom}T00:00`;
  const rangeTo = `${store.dateTo}T23:59`;
  return {
    from: timeFrom.value && timeFrom.value > rangeFrom ? timeFrom.value : rangeFrom,
    to: timeTo.value && timeTo.value < rangeTo ? timeTo.value : rangeTo,
  };
}

const timeWindowInvalid = computed(() => {
  if (!timeEnabled.value) return false;
  const window = wallTimeWindow();
  return window.from > window.to;
});

// The backend receives UTC boundaries (NaiveDateTime needs seconds). The "to"
// minute is padded to its last nanosecond so an entry logged anywhere inside it
// still falls in range. A cleared picker falls back to that end of the selected
// day range.
function timeWindow(): { from: string | null; to: string | null } {
  if (!timeEnabled.value) return { from: null, to: null };
  const window = wallTimeWindow();
  return {
    from: pickerTimeToUtc(window.from),
    to: pickerTimeToUtc(window.to, true),
  };
}

watch(timeMode, (next, previous) => {
  timeFrom.value = convertPickerTime(timeFrom.value, previous, next);
  timeTo.value = convertPickerTime(timeTo.value, previous, next);
});

watch([query, isRegex, caseSensitive, timeEnabled, timeFrom, timeTo], () => {
  localStorage.setItem(
    FILTER_KEY,
    JSON.stringify({
      query: query.value,
      isRegex: isRegex.value,
      caseSensitive: caseSensitive.value,
      timeEnabled: timeEnabled.value,
      timeFrom: timeFrom.value,
      timeTo: timeTo.value,
    }),
  );
});
const searching = ref(false);
const cancelRequested = ref(false);
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
  visibleHits.value.map((hit) => formatLogTime(hit.timestamp, store.logOffset(hit.serviceId))),
);

const allQueries = computed(() => [...presets.value, ...store.config.savedQueries]);

// An action menu rather than a setting: it applies a query and drops straight
// back to its own label, so it always rests on the value no option carries.
const savedPick = ref('');
const savedQueryOptions = computed<SelectOption<string>[]>(() =>
  allQueries.value.map((saved, i) => ({ value: String(i), label: saved.name })),
);

function pickSavedQuery(index: string) {
  savedPick.value = '';
  const saved = allQueries.value[Number(index)];
  if (saved) applyQuery(saved);
}

function applyQuery(saved: SavedQuery) {
  query.value = saved.query;
  isRegex.value = saved.isRegex;
  // A query the builder wrote loads back into it, so picking a saved query
  // while the builder is open shows its criteria. One the builder cannot read
  // is a plain query, and the builder steps out of the way rather than sit
  // there showing criteria that have nothing to do with the field.
  if (showBuilder.value && saved.isRegex && parseQuery(saved.query)) builderReload.value++;
  else showBuilder.value = false;
}

// --- Query builder ---

const BUILDER_OPEN_KEY = 'loglooker.queryBuilderOpen';
const showBuilder = ref(localStorage.getItem(BUILDER_OPEN_KEY) === '1');

// The builder reads the query field once, when it mounts. Bumping this remounts
// it so it reads the field again - only when a saved query is applied over it,
// never on the edits it makes itself.
const builderReload = ref(0);

// What the builder may read back as its criteria. A field in literal mode holds
// text, not a pattern the builder could have written, whatever it looks like.
const builderSource = computed(() => (isRegex.value ? query.value : ''));

watch(showBuilder, (open) => {
  localStorage.setItem(BUILDER_OPEN_KEY, open ? '1' : '0');
});

// The builder compiles its criteria to a plain regex; closing it leaves the
// compiled pattern in the field, free to be edited by hand. Its groups come
// along as sets, since the pattern alone does not say which capture groups
// belong together - they outlive the builder being closed, and only apply
// while the field still holds the pattern they describe.
const builderQuery = ref('');
const builderSets = ref<QuerySet[]>([]);

function onCompiled(compiled: string, sets: QuerySet[]) {
  query.value = compiled;
  isRegex.value = true;
  builderQuery.value = compiled;
  builderSets.value = sets;
}

// Store the current query under `name`, overwriting an existing saved query of
// the same name rather than adding a duplicate, then persist.
async function persistSavedQuery(name: string) {
  const entry = { name, query: query.value, isRegex: isRegex.value };
  const existing = store.config.savedQueries.findIndex((q) => q.name === name);
  if (existing === -1) {
    store.config.savedQueries.push(entry);
  } else {
    store.config.savedQueries[existing] = entry;
  }
  try {
    await store.saveConfig();
  } catch (e) {
    error.value = String(e);
  }
}

async function saveCurrentQuery() {
  const name = prompt('Name for this query:', query.value.slice(0, 40));
  if (!name) return;
  await persistSavedQuery(name);
}

// Save As opens a modal: type a fresh name, or click an existing saved query to
// overwrite it. The modal never silently clobbers - a name that matches an
// existing query is flagged as an overwrite before the user commits.
const showSaveAs = ref(false);
const saveAsName = ref('');

function openSaveAs() {
  saveAsName.value = '';
  showSaveAs.value = true;
}

// True when the typed name matches an existing saved query, so the modal can
// warn that saving will replace it rather than add a new one.
const saveAsClashes = computed(() =>
  store.config.savedQueries.some((q) => q.name === saveAsName.value.trim()),
);

async function confirmSaveAs() {
  const name = saveAsName.value.trim();
  if (!name) return;
  await persistSavedQuery(name);
  showSaveAs.value = false;
}

// Presets are backend built-ins and stay read-only; only the user's own saved
// queries can be renamed or removed.
const showManageQueries = ref(false);

async function renameSavedQuery(saved: SavedQuery) {
  const name = prompt('Rename query:', saved.name);
  if (name === null) return;
  const trimmed = name.trim();
  if (!trimmed || trimmed === saved.name) return;
  saved.name = trimmed;
  try {
    await store.saveConfig();
  } catch (e) {
    error.value = String(e);
  }
}

async function removeSavedQuery(saved: SavedQuery) {
  if (!confirm(`Remove saved query "${saved.name}"?`)) return;
  const i = store.config.savedQueries.indexOf(saved);
  if (i === -1) return;
  store.config.savedQueries.splice(i, 1);
  if (store.config.savedQueries.length === 0) showManageQueries.value = false;
  try {
    await store.saveConfig();
  } catch (e) {
    error.value = String(e);
  }
}

async function search(preview = false) {
  if (store.selectedServices.length === 0 || searching.value) return;
  if (timeWindowInvalid.value) {
    error.value = 'Time filter: "from" is after "to".';
    return;
  }
  const window = timeWindow();
  await runSearch(query.value, isRegex.value, caseSensitive.value, window.from, window.to, preview);
}

// The banner's "Run full search" repeats the previewed query, not the input box -
// the user may already be typing the next refinement there
async function continueFullSearch() {
  if (searching.value) return;
  await runSearch(
    searchedQuery.value,
    searchedIsRegex.value,
    searchedCaseSensitive.value,
    searchedTimeFrom.value,
    searchedTimeTo.value,
    false,
  );
}

// The stale-range banner refreshes the current result against the now-changed
// date range. It repeats the query these results came from (same reasoning as
// continueFullSearch), keeping the preview/full mode they were run in.
async function rerunForDateRange() {
  if (searching.value) return;
  await runSearch(
    searchedQuery.value,
    searchedIsRegex.value,
    searchedCaseSensitive.value,
    searchedTimeFrom.value,
    searchedTimeTo.value,
    searchedPreview.value,
  );
}

async function runSearch(
  submittedQuery: string,
  submittedIsRegex: boolean,
  submittedCaseSensitive: boolean,
  submittedTimeFrom: string | null,
  submittedTimeTo: string | null,
  preview: boolean,
) {
  searching.value = true;
  cancelRequested.value = false;
  progress.value = null;
  error.value = null;
  exportMsg.value = null;
  // The backend gives up the stored result the moment a search starts, so the
  // steps that were narrowing it are gone whichever way this one ends
  narrowSteps.value = [];
  // The previous result stays on screen (blurred) until the new one arrives
  try {
    const result = await invoke<SearchMeta>('search_logs', {
      request: {
        serviceIds: store.selectedServices.map((s) => s.id),
        dateFrom: store.dateFrom,
        dateTo: store.dateTo,
        timeFrom: submittedTimeFrom,
        timeTo: submittedTimeTo,
        query: submittedQuery,
        isRegex: submittedIsRegex,
        caseSensitive: submittedCaseSensitive,
        contextLines: contextLines.value,
      },
      previewLimit: preview ? PREVIEW_LIMIT : null,
    });
    meta.value = result;
    hits.value = [];
    expanded.value = new Set();
    sort.value = { key: 'time', ascending: false };
    searchedQuery.value = submittedQuery;
    // A hand-edited query no longer matches the builder tree, so its sets
    // would name capture groups that need not exist any more
    searchedSets.value = submittedQuery === builderQuery.value ? builderSets.value : [];
    searchedIsRegex.value = submittedIsRegex;
    searchedCaseSensitive.value = submittedCaseSensitive;
    searchedTimeFrom.value = submittedTimeFrom;
    searchedTimeTo.value = submittedTimeTo;
    searchedPreview.value = preview;
    searchedDateFrom.value = store.dateFrom;
    searchedDateTo.value = store.dateTo;
    await fillVisibleRows();
  } catch (e) {
    if (String(e) === 'Search cancelled') {
      // The backend freed the previous result when this search started, so
      // there is nothing left to page through - back to the empty state
      meta.value = null;
      hits.value = [];
      expanded.value = new Set();
    } else {
      error.value = String(e);
    }
  } finally {
    searching.value = false;
    cancelRequested.value = false;
  }
}

async function cancelSearch() {
  if (!searching.value || cancelRequested.value) return;
  cancelRequested.value = true;
  try {
    await invoke('cancel_search');
  } catch (e) {
    error.value = String(e);
  }
}

// --- Narrow: a second query run over the result already on screen ---

interface NarrowStep {
  query: string;
  isRegex: boolean;
  caseSensitive: boolean;
  invert: boolean;
}

const narrowQuery = ref('');
const narrowIsRegex = ref(false);
const narrowCaseSensitive = ref(false);
const narrowInvert = ref(false);
// The steps applied to the current result, oldest first. Only the last one can
// be taken back, so this doubles as the undo depth.
const narrowSteps = ref<NarrowStep[]>([]);

// A narrow only re-reads the entries the result already points at, never the
// files, so it runs in a fraction of the time repeating the search would. It
// borrows the search's busy state: the same overlay, progress and cancel apply.
async function narrowResults() {
  if (!meta.value || searching.value || !narrowQuery.value) return;
  const step: NarrowStep = {
    query: narrowQuery.value,
    isRegex: narrowIsRegex.value,
    caseSensitive: narrowCaseSensitive.value,
    invert: narrowInvert.value,
  };
  searching.value = true;
  cancelRequested.value = false;
  progress.value = null;
  error.value = null;
  exportMsg.value = null;
  try {
    meta.value = await invoke<SearchMeta>('narrow_search', { request: step });
    narrowSteps.value = [...narrowSteps.value, step];
    narrowQuery.value = '';
    await reloadRows();
  } catch (e) {
    // A cancelled narrow leaves the result exactly as it was - nothing to say
    if (String(e) !== 'Search cancelled') error.value = String(e);
  } finally {
    searching.value = false;
    cancelRequested.value = false;
  }
}

async function undoNarrow() {
  if (!meta.value || searching.value || narrowSteps.value.length === 0) return;
  error.value = null;
  try {
    meta.value = await invoke<SearchMeta>('undo_narrow');
    narrowSteps.value = narrowSteps.value.slice(0, -1);
    await reloadRows();
  } catch (e) {
    error.value = String(e);
  }
}

/** The loaded pages belong to a hit set that has just changed under them. */
async function reloadRows() {
  hits.value = [];
  expanded.value = new Set();
  await fillVisibleRows();
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

// Switching a group off can hide most of a loaded page, leaving too few rows to
// scroll - and scrolling is what pages the rest in. Keep pulling until enough
// rows survive the filter, or the result runs out.
const MIN_VISIBLE_ROWS = 100;

async function fillVisibleRows() {
  while (
    meta.value &&
    visibleHits.value.length < MIN_VISIBLE_ROWS &&
    hits.value.length < meta.value.totalHits
  ) {
    const before = hits.value.length;
    await loadMore();
    // No progress means another load is already running or one failed
    if (hits.value.length === before) return;
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

// The backend sorts by 'time', 'service', 'file', or any result column key
type SortField = string;

// Column key -> backend sort field. The line column sorts by its file rather
// than by its own content; a field column sorts by itself.
const SORT_FIELDS: Record<string, SortField> = {
  time: 'time',
  service: 'service',
  line: 'file',
};

/** The backend sort field a column header stands for. */
function sortFieldOf(key: string): SortField {
  return SORT_FIELDS[key] ?? key;
}

const baseColumns: DataTableColumn[] = [
  {
    key: 'time',
    label: 'Time',
    width: 248,
    fitContent: true,
    sortable: true,
    numeric: true,
    cellClass: 'text-gray-500 dark:text-gray-400 whitespace-nowrap',
  },
  { key: 'service', label: 'Service', width: 150, sortable: true },
  {
    key: 'line',
    label: 'Line',
    sortable: true,
    // break-words, not break-all: a wrapped log line stays readable when words
    // survive the wrap, and an unbreakable token (a URL, a stack frame) still
    // splits because it cannot fit a line on its own. Relaxed leading keeps the
    // wrapped lines apart.
    cellClass: 'text-gray-800 dark:text-gray-200 break-words leading-relaxed',
  },
];

// One column per field the result carries, between Service and Line. The set
// comes from the result itself (SearchMeta.fields), so a plugin's fields appear
// as columns without the app knowing any of them by name - and a field no hit
// filled in is not in there at all, so an empty column never gets rendered.
const columns = computed<DataTableColumn[]>(() => {
  const fields = meta.value?.fields ?? [];
  if (fields.length === 0) return baseColumns;
  const extra: DataTableColumn[] = fields.map((field) => ({
    key: field.key,
    label: field.label,
    width: 170,
    sortable: true,
    numeric: field.type === 'number',
    cellClass: 'text-gray-500 dark:text-gray-400',
  }));
  const at = baseColumns.findIndex((column) => column.key === 'line');
  const before = at === -1 ? baseColumns : baseColumns.slice(0, at);
  const after = at === -1 ? [] : baseColumns.slice(at);
  return [...before, ...extra, ...after];
});

// One-time migration: the hand-rolled search table stored its layout under its
// own key; the shared DataTable now owns it under the common prefix. The saved
// shape ({order, hidden, widths}, same column keys) is identical.
const LEGACY_COLUMNS_KEY = 'loglooker.searchColumns';
try {
  const legacy = localStorage.getItem(LEGACY_COLUMNS_KEY);
  if (legacy) {
    if (!localStorage.getItem('loglooker.table.search')) {
      localStorage.setItem('loglooker.table.search', legacy);
    }
    localStorage.removeItem(LEGACY_COLUMNS_KEY);
  }
} catch {
  // ignore storage failures
}

// Sorting is server-side: the header UI proposes the next state, and it is
// applied only once the backend has re-sorted the result
const sort = ref<SortState>({ key: 'time', ascending: false });

async function onSort(next: SortState) {
  if (!meta.value || searching.value) return;
  error.value = null;
  sort.value = next;
  try {
    await invoke('sort_search_hits', { field: sortFieldOf(next.key), ascending: next.ascending });
    expanded.value = new Set();
    hits.value = [];
    await fillVisibleRows();
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
const showRegexGuide = ref(false);
// The table is the chart's accessible twin: every charted value is readable here
const tab = ref<'table' | 'chart'>('table');
// The query the current result came from - the chart labels itself with it, and
// it must not follow the input box while the user types the next search.
// The flags travel with it into the raw viewer, which pre-applies the search.
const searchedQuery = ref('');
const searchedIsRegex = ref(false);
const searchedCaseSensitive = ref(false);
// The time window the current result was searched with, so the rerun banners
// repeat it rather than pick up edits made to the inputs since.
const searchedTimeFrom = ref<string | null>(null);
const searchedTimeTo = ref<string | null>(null);
// The current result came from a preview (tiny hit cap) - the banner offers to
// run it in full, and the config-cap warning must not fire for it
const searchedPreview = ref(false);
// The date range the current result was searched over. The filter line always
// shows the live store range, so if the user changes it on the Services page
// and comes back without re-searching, these results silently belong to the old
// range - dateRangeStale drives a warning to re-run.
const searchedDateFrom = ref('');
const searchedDateTo = ref('');

const dateRangeStale = computed(
  () =>
    meta.value !== null &&
    (searchedDateFrom.value !== store.dateFrom || searchedDateTo.value !== store.dateTo),
);

// --- Inline highlighting, coloured per capture group (slots shared with charts) ---

// Groups the user switched off in the legend: neither highlighted inline nor
// carried into the extracted value (preview, Copy and Export alike). Keyed by
// group name; pruned to the current result's groups whenever a search returns.
const disabledGroups = ref<Set<string>>(new Set());

function toggleGroup(name: string) {
  const next = new Set(disabledGroups.value);
  if (next.has(name)) next.delete(name);
  else next.add(name);
  disabledGroups.value = next;
}

// The builder groups behind the searched query, so a whole set can be switched
// off at once instead of clicking its criteria one by one
const searchedSets = ref<QuerySet[]>([]);

function setIsOff(names: string[]): boolean {
  return names.every((name) => disabledGroups.value.has(name));
}

// Off unless the whole set is already off - a half-on set switches fully off
// first, which is what the single visible state of the chip promises
function toggleSet(names: string[]) {
  const next = new Set(disabledGroups.value);
  const turnOff = !setIsOff(names);
  for (const name of names) {
    if (turnOff) next.add(name);
    else next.delete(name);
  }
  disabledGroups.value = next;
}

// A hit whose every matched group is switched off carries no highlight left, so
// it drops out of the table entirely - the same rows Copy and Export leave out.
// Hits from a query without named groups always stay.
const visibleHits = computed(() => {
  const off = disabledGroups.value;
  if (off.size === 0) return hits.value;
  return hits.value.filter(
    (hit) => hit.matchedGroups.length === 0 || hit.matchedGroups.some((name) => !off.has(name)),
  );
});

const hiddenHits = computed(() => hits.value.length - visibleHits.value.length);

// Rows are expanded by index into the rendered list, and hiding groups shifts
// every index below the first hidden hit - re-loading keeps rows in sync
watch(disabledGroups, () => {
  expanded.value = new Set();
  fillVisibleRows();
});

watch(
  () => meta.value?.groupNames,
  (names) => {
    if (!names) return;
    const live = new Set(names);
    const pruned = new Set([...disabledGroups.value].filter((g) => live.has(g)));
    if (pruned.size !== disabledGroups.value.size) disabledGroups.value = pruned;
  },
);

// A literal term as a pattern the alternation below can hold
function highlightSource(query: string, isRegex: boolean): string {
  return isRegex ? query : query.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

// Highlighting takes a single pattern, so the terms a narrowed row was kept for
// are alternated onto the searched query - each one colours where it matched.
// Their named groups (if any) come after the query's, so the group colours the
// legend shows never move. An excluding step colours nothing: it kept the rows
// its term is absent from.
const highlighter = computed(() => {
  const plain = () =>
    buildHighlighter(
      searchedQuery.value,
      searchedIsRegex.value,
      searchedCaseSensitive.value,
      disabledGroups.value,
    );
  const extra = narrowSteps.value.filter((step) => !step.invert);
  if (extra.length === 0) return plain();
  const sources = [
    ...(searchedQuery.value ? [highlightSource(searchedQuery.value, searchedIsRegex.value)] : []),
    ...extra.map((step) => highlightSource(step.query, step.isRegex)),
  ];
  // Case folding is a flag on the whole pattern and JS regex has no inline
  // (?i:...) to scope it per term, so a mix falls back to insensitive: that
  // over-colours at worst, where the other way round would miss matches.
  const caseSensitive = searchedCaseSensitive.value && extra.every((step) => step.caseSensitive);
  const combined = sources.map((source) => `(?:${source})`).join('|');
  return buildHighlighter(combined, true, caseSensitive, disabledGroups.value) ?? plain();
});

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

// --- Extract only: show (and export) just the matched part per line ---

const EXTRACT_KEY = 'loglooker.extractOnly';
const extractOnly = ref(localStorage.getItem(EXTRACT_KEY) === '1');
watch(extractOnly, (on) => localStorage.setItem(EXTRACT_KEY, on ? '1' : '0'));

// Extraction only makes sense for a regex search; a substring would just yield
// itself. When off, extract requests fall back to whole lines.
const canExtract = computed(() => searchedIsRegex.value && searchedQuery.value !== '');
const extractActive = computed(() => extractOnly.value && canExtract.value);

const extractor = computed(() =>
  canExtract.value
    ? buildExtractorParts(
        searchedQuery.value,
        searchedIsRegex.value,
        searchedCaseSensitive.value,
        disabledGroups.value,
      )
    : null,
);

// Same reasoning as partsCache: rows re-render far more often than the query changes.
// undefined = not yet computed; null = the line did not match (fall back to the
// whole-line highlight); an array is the coloured extract to render.
let extractCache = new WeakMap<SearchHit, HighlightPart[] | null>();
watch(extractor, () => {
  extractCache = new WeakMap();
});

function extractedParts(hit: SearchHit): HighlightPart[] | null {
  const fn = extractor.value;
  if (!fn) return null;
  let value = extractCache.get(hit);
  if (value === undefined) {
    value = fn(hit.line);
    extractCache.set(hit, value);
  }
  return value;
}

// --- Single row: never let one hit wrap over several lines ---

// Even clamped to ROW_CHARS, a long line wraps into a handful of visual lines, so
// only a few hits fit on screen. With this on a collapsed row is exactly one line
// tall, cut with an ellipsis; expanding the row still shows the line in full.
const SINGLE_ROW_KEY = 'loglooker.singleRow';
const singleRow = ref(localStorage.getItem(SINGLE_ROW_KEY) === '1');
watch(singleRow, (on) => localStorage.setItem(SINGLE_ROW_KEY, on ? '1' : '0'));

// --- What a collapsed row shows of the line ---

// A single line can be megabytes (a serialised request payload). Rendered whole
// it wraps into hundreds of visual lines, so one hit grows taller than the
// viewport and pushes every other hit off screen. A collapsed row therefore
// shows a window around the first match; expanding the row (or the raw viewer)
// still shows the line in full.
const ROW_CHARS = 600;
// Characters of context kept before the first highlighted part, so the match
// does not sit flush against the leading ellipsis
const ROW_LEAD = 80;

/** `parts` cut down to a ROW_CHARS window around the first coloured part, with
 * ellipsis markers for what was left out. Returned as-is when it already fits. */
function clampParts(parts: HighlightPart[]): HighlightPart[] {
  const total = parts.reduce((sum, part) => sum + part.text.length, 0);
  if (total <= ROW_CHARS) return parts;

  let firstMatch = 0;
  let offset = 0;
  for (const part of parts) {
    if (part.slot !== null) {
      firstMatch = offset;
      break;
    }
    offset += part.text.length;
  }
  const start = Math.max(0, firstMatch - ROW_LEAD);
  const end = Math.min(total, start + ROW_CHARS);

  const out: HighlightPart[] = [];
  if (start > 0) out.push({ text: '...', slot: null });
  offset = 0;
  for (const part of parts) {
    const partStart = offset;
    offset += part.text.length;
    if (offset <= start || partStart >= end) continue;
    const text = part.text.slice(
      Math.max(0, start - partStart),
      Math.min(part.text.length, end - partStart),
    );
    if (text) out.push({ ...part, text });
  }
  if (end < total) {
    out.push({ text: ` ... (+${(total - end).toLocaleString()} chars)`, slot: null });
  }
  return out;
}

// Same reasoning as partsCache: the clamped window only changes with the query
// or the extract toggle, while rows re-render on every scroll
let displayCache = new WeakMap<SearchHit, HighlightPart[]>();
watch([highlighter, extractor, extractActive], () => {
  displayCache = new WeakMap();
});

/** The coloured parts a collapsed row renders for a hit. */
function displayParts(hit: SearchHit): HighlightPart[] {
  let parts = displayCache.get(hit);
  if (!parts) {
    const extracted = extractActive.value ? extractedParts(hit) : null;
    parts = clampParts(extracted ?? lineParts(hit));
    displayCache.set(hit, parts);
  }
  return parts;
}

// --- Copy / export the whole result, one line per hit ---

const exportBusy = ref(false);
const exportMsg = ref<string | null>(null);

function exportRequest(path: string | null): ExportRequest {
  return {
    query: searchedQuery.value,
    isRegex: searchedIsRegex.value,
    caseSensitive: searchedCaseSensitive.value,
    extract: extractActive.value,
    path,
    maxLines: path ? null : CLIPBOARD_MAX_LINES,
    excludeGroups: [...disabledGroups.value],
  };
}

async function copyMatches() {
  if (!meta.value || exportBusy.value) return;
  exportBusy.value = true;
  exportMsg.value = null;
  error.value = null;
  try {
    const result = await invoke<ExportResult>('export_matches', { request: exportRequest(null) });
    await navigator.clipboard.writeText(result.text ?? '');
    exportMsg.value = result.truncated
      ? `Copied ${result.exported.toLocaleString()} of ${result.total.toLocaleString()} lines (clipboard limit ${CLIPBOARD_MAX_LINES.toLocaleString()}).`
      : `Copied ${result.exported.toLocaleString()} lines.`;
  } catch (e) {
    error.value = String(e);
  } finally {
    exportBusy.value = false;
  }
}

async function exportToFile() {
  if (!meta.value || exportBusy.value) return;
  error.value = null;
  let path: string | null;
  try {
    path = await save({
      defaultPath: extractActive.value ? 'matches.txt' : 'log-lines.txt',
      filters: [{ name: 'Text', extensions: ['txt'] }],
    });
  } catch (e) {
    error.value = String(e);
    return;
  }
  if (!path) return;
  exportBusy.value = true;
  exportMsg.value = null;
  try {
    const result = await invoke<ExportResult>('export_matches', { request: exportRequest(path) });
    exportMsg.value = `Exported ${result.exported.toLocaleString()} lines to ${result.savedPath}.`;
  } catch (e) {
    error.value = String(e);
  } finally {
    exportBusy.value = false;
  }
}

// Colour legend for the searched query's capture groups, shown above the results
const groupChips = computed(() =>
  (meta.value?.groupNames ?? []).map((name, i) => ({ name, slot: paletteSlot(i + 1) })),
);

// Set chips sit next to them and toggle every group of one builder set. Sets
// are pruned to the capture groups this result actually has; a set down to a
// single group would duplicate that group's own chip, so it is left out.
const setChips = computed(() => {
  const live = new Set(meta.value?.groupNames ?? []);
  return searchedSets.value
    .map((set) => ({ ...set, names: set.names.filter((name) => live.has(name)) }))
    .filter((set) => set.names.length > 1);
});

const progressText = computed(() => {
  const p = progress.value;
  if (!p) return 'Starting search...';
  if (p.phase === 'narrowing') {
    return (
      `Narrowing ${p.filesDone.toLocaleString()}/${p.filesTotal.toLocaleString()} entries - ` +
      `${p.hits.toLocaleString()} kept`
    );
  }
  if (p.phase === 'sorting') return `Sorting ${p.hits.toLocaleString()} hits...`;
  if (p.filesTotal === 0) return 'No cached files in range...';
  return (
    `Scanning files ${p.filesDone}/${p.filesTotal} - ` +
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
      <button class="ml-4 shrink-0 hover:underline" @click="error = null">Dismiss</button>
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
        @keydown.enter="search()"
      />
      <button
        class="px-4 py-1.5 text-xs font-semibold rounded-md border border-blue-500 text-blue-600 dark:text-blue-400 hover:bg-blue-50 dark:hover:bg-blue-900/30 disabled:opacity-50 transition-colors"
        :disabled="searching || store.selectedServices.length === 0"
        :title="`Scan only until the first ${PREVIEW_LIMIT} hits - quick way to check the query before a full search`"
        @click="search(true)"
      >
        Preview
      </button>
      <button
        class="px-4 py-1.5 text-xs font-semibold rounded-md bg-blue-500 text-white hover:bg-blue-600 disabled:opacity-50 transition-colors"
        :disabled="searching || store.selectedServices.length === 0"
        @click="search()"
      >
        {{ searching ? 'Searching...' : 'Search' }}
      </button>
    </div>

    <div class="flex items-center gap-4 mb-2 text-xs text-gray-600 dark:text-gray-400">
      <BaseCheckbox v-model="isRegex" :disabled="showBuilder"> Regex </BaseCheckbox>
      <BaseCheckbox v-model="caseSensitive"> Case sensitive </BaseCheckbox>
      <button
        class="w-5 h-5 flex items-center justify-center rounded-full border border-gray-300 dark:border-gray-600 text-gray-500 dark:text-gray-400 hover:text-blue-600 dark:hover:text-blue-400 hover:border-blue-400 dark:hover:border-blue-600"
        title="Regex syntax guide and examples"
        @click="showRegexGuide = true"
      >
        ?
      </button>
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
        <BaseSelect v-model="contextLines" :options="CONTEXT_OPTIONS" />
      </label>

      <BaseSelect
        v-model="savedPick"
        :options="savedQueryOptions"
        placeholder="Saved queries..."
        @change="pickSavedQuery"
      />
      <button
        class="px-2 py-1 border border-gray-300 dark:border-gray-600 rounded hover:text-gray-800 dark:hover:text-gray-200 disabled:opacity-50"
        :disabled="!query"
        @click="saveCurrentQuery"
      >
        Save query
      </button>
      <button
        class="px-2 py-1 border border-gray-300 dark:border-gray-600 rounded hover:text-gray-800 dark:hover:text-gray-200 disabled:opacity-50"
        :disabled="!query"
        @click="openSaveAs"
      >
        Save as
      </button>
      <button
        class="px-2 py-1 border rounded disabled:opacity-50"
        :class="
          showManageQueries
            ? 'border-blue-500 text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30'
            : 'border-gray-300 dark:border-gray-600 hover:text-gray-800 dark:hover:text-gray-200'
        "
        :disabled="store.config.savedQueries.length === 0"
        @click="showManageQueries = !showManageQueries"
      >
        Manage
      </button>

      <span class="ml-auto">
        {{ store.selectedServices.length }} services selected ({{ store.environment }}),
        {{ store.dateFrom }} → {{ store.dateTo }}
        <router-link to="/" class="text-blue-500 hover:underline">change</router-link>
      </span>
    </div>

    <div class="flex items-center gap-3 mb-4 text-xs text-gray-600 dark:text-gray-400">
      <BaseCheckbox
        v-model="timeEnabled"
        title="Limit results to entries between two instants inside the selected date range"
      >
        Time filter ({{ timeMode === 'utc' ? 'UTC' : 'local' }})
      </BaseCheckbox>
      <template v-if="timeEnabled">
        <div class="flex items-center gap-1.5">
          from
          <DateTimePicker v-model="timeFrom" aria-label="Čas od" :utc="timeMode === 'utc'" />
        </div>
        <div class="flex items-center gap-1.5">
          to
          <DateTimePicker
            v-model="timeTo"
            aria-label="Čas do"
            align="right"
            :utc="timeMode === 'utc'"
          />
        </div>
        <span v-if="timeWindowInvalid" class="text-red-600 dark:text-red-400">
          "from" is after "to"
        </span>
      </template>
    </div>

    <Teleport to="body">
      <div
        v-if="showManageQueries && store.config.savedQueries.length"
        class="fixed inset-0 z-50 flex items-center justify-center bg-black/30 backdrop-blur-sm"
        @click.self="showManageQueries = false"
      >
        <div
          class="bg-white dark:bg-gray-800 rounded-lg shadow-xl w-full max-w-lg mx-4 max-h-[80vh] flex flex-col"
        >
          <div
            class="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700"
          >
            <h2 class="text-sm font-medium text-gray-700 dark:text-gray-200">Saved queries</h2>
            <button
              class="text-gray-400 hover:text-gray-700 dark:hover:text-gray-200"
              @click="showManageQueries = false"
            >
              &times;
            </button>
          </div>
          <ul class="flex flex-col gap-1 px-5 py-3 overflow-y-auto">
            <li
              v-for="(saved, i) in store.config.savedQueries"
              :key="i"
              class="flex items-center gap-2 text-sm"
            >
              <span class="shrink-0 whitespace-nowrap">{{ saved.name }}</span>
              <code class="truncate min-w-0 text-xs text-gray-400 dark:text-gray-500">{{
                saved.query
              }}</code>
              <div class="ml-auto flex shrink-0 gap-1">
                <button
                  class="px-2 py-0.5 border border-gray-300 dark:border-gray-600 rounded hover:text-gray-800 dark:hover:text-gray-200"
                  @click="renameSavedQuery(saved)"
                >
                  Rename
                </button>
                <button
                  class="px-2 py-0.5 border border-gray-300 dark:border-gray-600 rounded text-red-600 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-900/30"
                  @click="removeSavedQuery(saved)"
                >
                  Remove
                </button>
              </div>
            </li>
          </ul>
        </div>
      </div>
    </Teleport>

    <Teleport to="body">
      <div
        v-if="showSaveAs"
        class="fixed inset-0 z-50 flex items-center justify-center bg-black/30 backdrop-blur-sm"
        @click.self="showSaveAs = false"
      >
        <div
          class="bg-white dark:bg-gray-800 rounded-lg shadow-xl w-full max-w-md mx-4 max-h-[80vh] flex flex-col"
        >
          <div
            class="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700"
          >
            <h2 class="text-sm font-medium text-gray-700 dark:text-gray-200">Save query as</h2>
            <button
              class="text-gray-400 hover:text-gray-700 dark:hover:text-gray-200"
              @click="showSaveAs = false"
            >
              &times;
            </button>
          </div>
          <div class="flex flex-col gap-3 px-5 py-4 overflow-y-auto">
            <input
              v-model="saveAsName"
              type="text"
              placeholder="New query name"
              class="w-full px-3 py-2 rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-sm"
              @keydown.enter="confirmSaveAs"
            />
            <p v-if="saveAsClashes" class="text-xs text-amber-600 dark:text-amber-400">
              This will overwrite the existing query "{{ saveAsName.trim() }}".
            </p>
            <div v-if="store.config.savedQueries.length">
              <p class="text-xs text-gray-500 dark:text-gray-400 mb-1">
                Or overwrite an existing query:
              </p>
              <ul class="flex flex-col gap-0.5 max-h-52 overflow-y-auto">
                <li v-for="(saved, i) in store.config.savedQueries" :key="i">
                  <button
                    class="w-full flex items-center gap-2 text-left text-sm px-2 py-1 rounded hover:bg-gray-100 dark:hover:bg-gray-700"
                    :class="
                      saved.name === saveAsName.trim()
                        ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-600 dark:text-blue-400'
                        : ''
                    "
                    @click="saveAsName = saved.name"
                  >
                    <span class="shrink-0 whitespace-nowrap">{{ saved.name }}</span>
                    <code class="truncate min-w-0 text-xs text-gray-400 dark:text-gray-500">{{
                      saved.query
                    }}</code>
                  </button>
                </li>
              </ul>
            </div>
          </div>
          <div
            class="flex justify-end gap-2 px-5 py-3 border-t border-gray-200 dark:border-gray-700"
          >
            <button
              class="px-3 py-1 border border-gray-300 dark:border-gray-600 rounded hover:text-gray-800 dark:hover:text-gray-200"
              @click="showSaveAs = false"
            >
              Cancel
            </button>
            <button
              class="px-3 py-1 rounded bg-blue-600 text-white hover:bg-blue-700 disabled:opacity-50"
              :disabled="!saveAsName.trim()"
              @click="confirmSaveAs"
            >
              {{ saveAsClashes ? 'Overwrite' : 'Save' }}
            </button>
          </div>
        </div>
      </div>
    </Teleport>

    <QueryBuilder
      v-if="showBuilder"
      :key="builderReload"
      :query="builderSource"
      @compiled="onCompiled"
      @search="search()"
    />

    <div
      v-if="dateRangeStale"
      class="mb-2 px-4 py-2 rounded-md bg-amber-50 dark:bg-amber-900/30 border border-amber-200 dark:border-amber-800 text-xs text-amber-700 dark:text-amber-400 flex items-center justify-between gap-3"
    >
      <span>
        These results cover {{ searchedDateFrom }} → {{ searchedDateTo }}, but the selected range is
        now {{ store.dateFrom }} → {{ store.dateTo }}. Search again to update them.
      </span>
      <button
        class="shrink-0 px-2 py-1 rounded bg-amber-500 text-white hover:bg-amber-600 disabled:opacity-50"
        :disabled="searching || store.selectedServices.length === 0"
        @click="rerunForDateRange"
      >
        Search again
      </button>
    </div>

    <div
      v-if="meta"
      class="mb-2 text-xs text-gray-500 dark:text-gray-400 flex items-center gap-3"
      :class="searching ? 'opacity-50' : ''"
    >
      <span>
        {{ meta.totalHits.toLocaleString() }} hits - {{ meta.filesScanned }} files,
        {{ meta.linesScanned.toLocaleString() }} lines in {{ meta.durationMs }} ms
      </span>
      <template v-if="searchedPreview">
        <span
          class="text-blue-600 dark:text-blue-400"
          :title="
            meta.truncated
              ? 'The preview stopped at the first hits. Refine the query and preview again, or run the full search.'
              : 'The preview already found every match, so this result is complete.'
          "
        >
          {{
            meta.truncated
              ? `preview - first ${meta.totalHits.toLocaleString()} hits, more may exist`
              : 'preview - all matches found, result is complete'
          }}
        </span>
        <button
          v-if="meta.truncated"
          class="px-2 py-1 rounded bg-blue-500 text-white hover:bg-blue-600 disabled:opacity-50"
          :disabled="searching"
          title="Run the previewed query again without the preview limit"
          @click="continueFullSearch"
        >
          Run full search
        </button>
      </template>
      <span
        v-else-if="meta.truncated"
        class="text-amber-600 dark:text-amber-400"
        title="The search stopped at the configured hit cap. Narrow the query or date range, or raise the cap in the memory settings (top right)."
      >
        capped at {{ meta.totalHits.toLocaleString() }} hits - result is incomplete
      </span>
      <button
        v-for="chip in groupChips"
        :key="chip.name"
        type="button"
        class="flex items-center gap-1.5 rounded px-1 -mx-0.5 text-gray-600 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700/50"
        :class="disabledGroups.has(chip.name) ? 'opacity-40' : ''"
        :title="
          disabledGroups.has(chip.name)
            ? `Group '${chip.name}' is off - click to highlight it again, bring back the lines only it matched, and include it in Extract match`
            : `Group '${chip.name}' - click to stop highlighting it, hide the lines only it matched, and leave it out of Extract match`
        "
        @click="toggleGroup(chip.name)"
      >
        <span
          class="w-2.5 h-2.5 rounded-sm shrink-0 border"
          :style="{
            borderColor: `var(--chart-${chip.slot})`,
            backgroundColor: disabledGroups.has(chip.name)
              ? 'transparent'
              : `var(--chart-${chip.slot})`,
          }"
        />
        <span
          class="truncate max-w-[10rem]"
          :class="disabledGroups.has(chip.name) ? 'line-through' : ''"
          >{{ chip.name }}</span
        >
      </button>
      <button
        v-for="set in setChips"
        :key="`set-${set.label}`"
        type="button"
        class="flex items-center gap-1.5 rounded-full border px-2 text-gray-600 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700/50"
        :class="setIsOff(set.names) ? 'opacity-40' : ''"
        :style="{ borderColor: `var(--chart-${set.slot})` }"
        :title="
          setIsOff(set.names)
            ? `Set '${set.label}' is off - click to highlight its ${set.names.length} groups and include them in Extract match`
            : `Set '${set.label}' (${set.names.join(', ')}) - click to switch the whole set off, leaving it out of highlights and Extract match`
        "
        @click="toggleSet(set.names)"
      >
        <span class="truncate max-w-[10rem]" :class="setIsOff(set.names) ? 'line-through' : ''">{{
          set.label
        }}</span>
      </button>
      <span v-if="tab === 'table' && hiddenHits > 0" class="text-gray-500 dark:text-gray-400">
        {{ hiddenHits.toLocaleString() }} hidden - only groups that are switched off matched
      </span>
      <span v-if="tab === 'table' && hits.length < meta.totalHits">
        showing {{ visibleHits.length.toLocaleString() }} (scroll to load more)
      </span>

      <BaseCheckbox
        v-if="canExtract"
        v-model="extractOnly"
        title="Show only the matched part of each line (the first capture group)"
      >
        Extract match
      </BaseCheckbox>
      <BaseCheckbox
        v-model="singleRow"
        title="Keep every hit one line tall, cut with an ellipsis. Click the row to see the whole line."
      >
        Single row
      </BaseCheckbox>
      <button
        v-if="meta.totalHits > 0"
        class="px-2 py-1 border border-gray-300 dark:border-gray-600 rounded hover:text-gray-800 dark:hover:text-gray-200 disabled:opacity-50"
        :disabled="exportBusy"
        :title="
          extractActive
            ? 'Copy the extracted match of every hit, one per line'
            : 'Copy every matching line, one per line'
        "
        @click="copyMatches"
      >
        Copy
      </button>
      <button
        v-if="meta.totalHits > 0"
        class="px-2 py-1 border border-gray-300 dark:border-gray-600 rounded hover:text-gray-800 dark:hover:text-gray-200 disabled:opacity-50"
        :disabled="exportBusy"
        title="Export all matches to a text file, one per line"
        @click="exportToFile"
      >
        Export...
      </button>

      <div
        class="ml-auto flex items-center rounded-md border border-gray-300 dark:border-gray-600 overflow-hidden"
      >
        <button
          v-for="option in ['table', 'chart'] as const"
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
    </div>

    <div
      v-if="meta"
      class="mb-2 flex flex-wrap items-center gap-2 text-xs text-gray-600 dark:text-gray-400"
      :class="searching ? 'opacity-50' : ''"
    >
      <input
        v-model="narrowQuery"
        type="text"
        spellcheck="false"
        placeholder="Narrow these results - search again inside the hits above..."
        class="flex-1 min-w-[16rem] px-3 py-1 text-xs font-mono rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800"
        title="Runs over the entries of the current result only, so it costs a fraction of a full search"
        @keydown.enter="narrowResults"
      />
      <BaseCheckbox v-model="narrowIsRegex"> Regex </BaseCheckbox>
      <BaseCheckbox v-model="narrowCaseSensitive"> Case sensitive </BaseCheckbox>
      <BaseCheckbox
        v-model="narrowInvert"
        title="Keep the entries this term is absent from instead of the ones it matches"
      >
        Exclude
      </BaseCheckbox>
      <button
        class="px-3 py-1 font-semibold rounded-md border border-blue-500 text-blue-600 dark:text-blue-400 hover:bg-blue-50 dark:hover:bg-blue-900/30 disabled:opacity-50 transition-colors"
        :disabled="searching || !narrowQuery"
        @click="narrowResults"
      >
        Narrow
      </button>
      <template v-if="narrowSteps.length">
        <span
          v-for="(step, i) in narrowSteps"
          :key="i"
          class="max-w-[16rem] truncate px-2 py-0.5 rounded-full border font-mono"
          :class="
            step.invert
              ? 'border-red-300 dark:border-red-800 text-red-600 dark:text-red-400'
              : 'border-blue-300 dark:border-blue-800 text-blue-600 dark:text-blue-400'
          "
          :title="`${step.invert ? 'Excluding' : 'Matching'} ${step.query}${step.isRegex ? ' (regex)' : ''}${step.caseSensitive ? ', case sensitive' : ''}`"
        >
          {{ step.invert ? '-' : '+' }} {{ step.query }}
        </span>
        <button
          class="px-2 py-1 border border-gray-300 dark:border-gray-600 rounded hover:text-gray-800 dark:hover:text-gray-200 disabled:opacity-50"
          :disabled="searching"
          title="Take back the last narrowing step and bring its hits back"
          @click="undoNarrow"
        >
          Undo narrow
        </button>
      </template>
    </div>

    <div
      v-if="exportMsg"
      class="mb-2 px-3 py-1.5 rounded-md bg-green-50 dark:bg-green-900/30 border border-green-200 dark:border-green-800 text-xs text-green-700 dark:text-green-400 flex items-center justify-between gap-3"
    >
      <span class="break-all">{{ exportMsg }}</span>
      <button class="shrink-0 hover:underline" @click="exportMsg = null">Dismiss</button>
    </div>

    <div v-if="meta" class="relative">
      <SearchChart v-if="tab === 'chart'" :query="searchedQuery" />

      <div
        v-if="tab === 'table'"
        class="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden"
      >
        <DataTable
          table-id="search"
          :columns="columns"
          :rows="visibleHits"
          table-class="min-w-[48rem] font-mono"
          scroll-class="max-h-[calc(100vh-260px)]"
          clickable-rows
          row-class="align-top"
          :sort="sort"
          :loading="loadingMore && hits.length === 0"
          :expanded="(_, i) => expanded.has(i)"
          @update:sort="onSort"
          @row-click="(_, i) => toggleExpanded(i)"
          @scroll="onResultsScroll"
        >
          <template #time="{ index }">
            <span :title="times[index].text + (times[index].zone ? ` (${times[index].zone})` : '')">
              {{ times[index].text }}
              <span v-if="times[index].zone" class="text-gray-400 dark:text-gray-500"
                >({{ times[index].zone }})</span
              >
            </span>
          </template>
          <template #service="{ row: hit }">
            <span
              class="inline-block px-1.5 py-0.5 text-[10px] font-semibold rounded"
              :title="serviceNames[hit.serviceId] ?? hit.serviceId"
              :class="
                hit.serviceId.startsWith('production/')
                  ? 'bg-red-100 dark:bg-red-900/30 text-red-700 dark:text-red-400'
                  : 'bg-blue-100 dark:bg-blue-900/30 text-blue-700 dark:text-blue-400'
              "
            >
              {{ serviceNames[hit.serviceId] ?? hit.serviceId }}
            </span>
          </template>
          <!-- One slot per plugin-declared field, keyed by its column key -->
          <template
            v-for="field in meta?.fields ?? []"
            #[field.key]="{ row: hit }"
            :key="field.key"
          >
            <div v-if="hit.fields[field.key]" class="truncate" :title="hit.fields[field.key]">
              {{ hit.fields[field.key] }}
            </div>
          </template>
          <template #line="{ row: hit }">
            <div :class="singleRow ? 'flex items-baseline gap-1' : ''">
              <span :class="singleRow ? 'min-w-0 truncate' : ''">
                <!-- No whitespace between the parts: Vue condenses the newlines
                     of a formatted template into real spaces, which would pad
                     every highlight and print a line the log never contained -->
                <template v-for="(part, p) in displayParts(hit)" :key="p"><span
                  v-if="part.slot !== null"
                  :class="partClass(part)"
                  :style="partStyle(part)"
                >{{ part.text }}</span><template v-else>{{ part.text }}</template></template>
              </span>
              <button
                class="text-gray-400 text-[10px] whitespace-nowrap hover:text-blue-500 hover:underline"
                :class="singleRow ? 'shrink-0' : ''"
                title="Open the raw file at this line"
                @click.stop="rawView = hit"
              >
                - {{ hit.file }}:{{ hit.lineNumber }}
              </button>
            </div>
          </template>
          <template #expansion="{ row: hit }">
            <div class="px-3 py-2 bg-gray-50 dark:bg-gray-900/50">
              <pre
                class="max-h-[60vh] overflow-auto whitespace-pre-wrap break-words text-[11px] leading-relaxed"
              ><span class="text-gray-400">{{ hit.contextBefore.join('\n') }}</span>
  <span class="text-gray-900 dark:text-gray-100 font-semibold"><template
    v-for="(part, p) in lineParts(hit)"
    :key="p"
  ><span
    v-if="part.slot !== null"
    :class="partClass(part)"
    :style="partStyle(part)"
  >{{ part.text }}</span><template v-else>{{ part.text }}</template></template></span>
  <span class="text-gray-400">{{ hit.contextAfter.join('\n') }}</span></pre>
              <button
                class="mt-2 px-2 py-1 text-[10px] border border-gray-300 dark:border-gray-600 rounded text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200"
                @click.stop="rawView = hit"
              >
                Open raw file at line {{ hit.lineNumber.toLocaleString() }}
              </button>
            </div>
          </template>
          <template #footer>
            <div v-if="loadingMore && hits.length > 0" class="py-3 text-center text-gray-400">
              <span class="inline-flex items-center gap-2">
                <Spinner size="sm" />
                Loading more ({{ hits.length.toLocaleString() }}/{{
                  meta.totalHits.toLocaleString()
                }})...
              </span>
            </div>
          </template>
          <template #empty> No matches. </template>
        </DataTable>
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
          <button
            class="mt-1 px-3 py-1 text-xs border border-gray-300 dark:border-gray-600 rounded-md text-gray-600 dark:text-gray-400 hover:text-red-600 dark:hover:text-red-400 disabled:opacity-50"
            :disabled="cancelRequested"
            @click="cancelSearch"
          >
            {{ cancelRequested ? 'Cancelling...' : 'Cancel' }}
          </button>
        </div>
      </div>
    </div>

    <div v-else-if="searching" class="py-16 flex flex-col items-center gap-3 text-gray-400 text-sm">
      <Spinner />
      <div>{{ progressText }}</div>
      <div v-if="progress?.currentFile" class="text-xs font-mono">
        {{ progress.currentFile }}
      </div>
      <button
        class="px-3 py-1 text-xs border border-gray-300 dark:border-gray-600 rounded-md text-gray-600 dark:text-gray-400 hover:text-red-600 dark:hover:text-red-400 disabled:opacity-50"
        :disabled="cancelRequested"
        @click="cancelSearch"
      >
        {{ cancelRequested ? 'Cancelling...' : 'Cancel' }}
      </button>
    </div>

    <div v-else class="py-16 text-center text-gray-400 text-sm">
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

    <RegexGuide v-if="showRegexGuide" @close="showRegexGuide = false" />
  </div>
</template>
