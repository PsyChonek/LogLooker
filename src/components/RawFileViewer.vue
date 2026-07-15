<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { formatBytes } from '@/stores/appStore';
import { buildHighlighter, slotClass, type HighlightPart } from '@/utils/regexQuery';
import type { RawFileInfo, RawSearchResult } from '@/types';

// Rows are a fixed height and never wrap, so the whole file can be virtualized:
// only the visible slice is in the DOM, and only the chunks it needs cross IPC.
const ROW_HEIGHT = 18;
const CHUNK = 1000;
const OVERSCAN = 20;
// Lines of headroom kept above the target line when jumping to it
const HEADROOM = 5;
const SEARCH_DEBOUNCE_MS = 300;

// `line` is the matched line to jump to and highlight; omitted when the file is
// opened from the Files view, where there is nothing to jump to.
// `windowed` renders the viewer as the whole window content instead of a modal.
// The `initial*` props carry the search that produced the hit, so the in-file
// search opens already applied.
const props = defineProps<{
  serviceId: string;
  serviceName: string;
  file: string;
  line?: number;
  windowed?: boolean;
  initialQuery?: string;
  initialIsRegex?: boolean;
  initialCaseSensitive?: boolean;
}>();

const emit = defineEmits<{ close: [] }>();

const info = ref<RawFileInfo | null>(null);
const error = ref<string | null>(null);
const loading = ref(true);
const lines = ref(new Map<number, string>());
const requestedChunks = new Set<number>();

const viewport = ref<HTMLElement | null>(null);
const scrollTop = ref(0);
const viewportHeight = ref(0);

const totalLines = computed(() => info.value?.totalLines ?? 0);
const gutterWidth = computed(() => `${String(totalLines.value).length + 1}ch`);

const range = computed(() => {
  const start = Math.max(0, Math.floor(scrollTop.value / ROW_HEIGHT) - OVERSCAN);
  const count = Math.ceil(viewportHeight.value / ROW_HEIGHT) + OVERSCAN * 2;
  return { start, end: Math.min(totalLines.value, start + count) };
});

const rows = computed(() => {
  const visible = [];
  for (let i = range.value.start; i < range.value.end; i++) {
    visible.push({ number: i + 1, text: lines.value.get(i) });
  }
  return visible;
});

async function loadChunks(start: number, end: number) {
  if (!info.value) return;
  const first = Math.floor(start / CHUNK);
  const last = Math.floor(Math.max(start, end - 1) / CHUNK);
  for (let chunk = first; chunk <= last; chunk++) {
    if (requestedChunks.has(chunk)) continue;
    requestedChunks.add(chunk);
    const offset = chunk * CHUNK;
    try {
      const page = await invoke<string[]>('get_raw_lines', {
        handle: info.value.handle,
        offset,
        limit: CHUNK,
      });
      page.forEach((text, i) => lines.value.set(offset + i, text));
    } catch (e) {
      requestedChunks.delete(chunk);
      error.value = String(e);
    }
  }
}

watch(range, ({ start, end }) => loadChunks(start, end));

function onScroll(event: Event) {
  scrollTop.value = (event.target as HTMLElement).scrollTop;
}

function jumpToLine(line: number) {
  if (!viewport.value) return;
  const top = Math.max(0, (line - 1 - HEADROOM) * ROW_HEIGHT);
  viewport.value.scrollTop = top;
  scrollTop.value = top;
}

function scrollToTargetLine() {
  if (props.line) jumpToLine(props.line);
}

// --- Search ---

const searchInput = ref<HTMLInputElement | null>(null);
const query = ref(props.initialQuery ?? '');
const isRegex = ref(props.initialIsRegex ?? false);
const caseSensitive = ref(props.initialCaseSensitive ?? false);
const searching = ref(false);
const searchError = ref<string | null>(null);
const result = ref<RawSearchResult | null>(null);
const current = ref(-1);
const showMatches = ref(false);
// The query the current result was produced with — inline highlighting must
// track it, not the input, which may already be ahead of the debounce
const executed = ref<{ query: string; isRegex: boolean; caseSensitive: boolean } | null>(null);

let searchTimer: ReturnType<typeof setTimeout> | undefined;
let searchToken = 0;
// When the viewer opens with both a target line and an inherited query, the
// first search must land on that line's match, not the file's first match
let jumpToTargetOnFirstSearch = props.line !== undefined && !!props.initialQuery;

watch([query, isRegex, caseSensitive], () => {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(runSearch, SEARCH_DEBOUNCE_MS);
});

async function runSearch() {
  const token = ++searchToken;
  const jumpToTarget = jumpToTargetOnFirstSearch;
  jumpToTargetOnFirstSearch = false;
  searchError.value = null;
  current.value = -1;
  if (!info.value || !query.value) {
    result.value = null;
    executed.value = null;
    return;
  }
  searching.value = true;
  try {
    const found = await invoke<RawSearchResult>('search_raw_file', {
      handle: info.value.handle,
      query: query.value,
      isRegex: isRegex.value,
      caseSensitive: caseSensitive.value,
    });
    if (token !== searchToken) return;
    executed.value = {
      query: query.value,
      isRegex: isRegex.value,
      caseSensitive: caseSensitive.value,
    };
    result.value = found;
    if (found.matches.length > 0) {
      if (jumpToTarget && props.line !== undefined) {
        const target = props.line;
        const index = found.matches.findIndex((m) => m.lineNumber >= target);
        gotoMatch(index >= 0 ? index : found.matches.length - 1);
      } else {
        gotoMatch(0);
      }
    }
  } catch (e) {
    if (token !== searchToken) return;
    searchError.value = String(e);
    result.value = null;
    executed.value = null;
  } finally {
    if (token === searchToken) searching.value = false;
  }
}

const currentLine = computed(() => result.value?.matches[current.value]?.lineNumber ?? null);

function gotoMatch(index: number) {
  const matches = result.value?.matches;
  if (!matches || matches.length === 0) return;
  const wrapped = ((index % matches.length) + matches.length) % matches.length;
  current.value = wrapped;
  jumpToLine(matches[wrapped].lineNumber);
  scrollMatchesToCurrent();
}

// Splits lines into plain and matched segments, coloured per capture group so
// hits keep the colours of the search that opened the viewer. Falls back to no
// highlighting when the query uses syntax JS regex lacks — the match list
// still works then.
const highlighter = computed(() => {
  const spec = executed.value;
  if (!spec) return null;
  return buildHighlighter(spec.query, spec.isRegex, spec.caseSensitive);
});

function highlightParts(text: string): HighlightPart[] {
  return highlighter.value?.parts(text) ?? [{ text, slot: null }];
}

// --- Matches pane (virtualized the same way as the file itself) ---

const matchesViewport = ref<HTMLElement | null>(null);
const matchesScrollTop = ref(0);
const MATCH_ROWS_VISIBLE = 40;

const matchRange = computed(() => {
  const total = result.value?.matches.length ?? 0;
  const start = Math.max(0, Math.floor(matchesScrollTop.value / ROW_HEIGHT) - OVERSCAN);
  return { start, end: Math.min(total, start + MATCH_ROWS_VISIBLE + OVERSCAN * 2) };
});

const matchRows = computed(() => {
  const matches = result.value?.matches ?? [];
  return matches
    .slice(matchRange.value.start, matchRange.value.end)
    .map((m, i) => ({ ...m, index: matchRange.value.start + i }));
});

function onMatchesScroll(event: Event) {
  matchesScrollTop.value = (event.target as HTMLElement).scrollTop;
}

function scrollMatchesToCurrent() {
  const pane = matchesViewport.value;
  if (!pane || current.value < 0) return;
  const top = current.value * ROW_HEIGHT;
  if (top < pane.scrollTop || top + ROW_HEIGHT > pane.scrollTop + pane.clientHeight) {
    pane.scrollTop = Math.max(0, top - pane.clientHeight / 2);
  }
}

// --- Window / lifecycle ---

async function openInWindow() {
  try {
    await invoke('open_raw_window', {
      serviceId: props.serviceId,
      serviceName: props.serviceName,
      file: props.file,
      line: props.line ?? null,
      query: query.value || null,
      isRegex: isRegex.value,
      caseSensitive: caseSensitive.value,
    });
    emit('close');
  } catch (e) {
    error.value = String(e);
  }
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') {
    if (document.activeElement === searchInput.value) {
      searchInput.value?.blur();
      return;
    }
    emit('close');
  } else if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'f') {
    event.preventDefault();
    searchInput.value?.focus();
    searchInput.value?.select();
  } else if (event.key === 'F3') {
    event.preventDefault();
    gotoMatch(current.value + (event.shiftKey ? -1 : 1));
  }
}

let observer: ResizeObserver | null = null;

onMounted(async () => {
  window.addEventListener('keydown', onKeydown);
  try {
    info.value = await invoke<RawFileInfo>('open_raw_file', {
      serviceId: props.serviceId,
      file: props.file,
    });
  } catch (e) {
    error.value = String(e);
    return;
  } finally {
    loading.value = false;
  }

  await nextTick();
  if (viewport.value) {
    viewportHeight.value = viewport.value.clientHeight;
    observer = new ResizeObserver(() => {
      if (viewport.value) viewportHeight.value = viewport.value.clientHeight;
    });
    observer.observe(viewport.value);
  }
  scrollToTargetLine();
  // Refs initialized from props do not fire the search watcher; an inherited
  // query must run once the file is open
  if (query.value) runSearch();
  await loadChunks(range.value.start, range.value.end);
});

onUnmounted(() => {
  window.removeEventListener('keydown', onKeydown);
  observer?.disconnect();
  const handle = info.value?.handle;
  if (handle !== undefined) {
    invoke('close_raw_file', { handle }).catch(() => {
      // Nothing to do — leftover temp files are swept on the next app start
    });
  }
});
</script>

<template>
  <div
    :class="
      windowed
        ? 'h-screen w-screen'
        : 'fixed inset-0 z-30 bg-black/40 flex items-center justify-center p-6'
    "
    @click.self="windowed || emit('close')"
  >
    <div
      class="w-full h-full flex flex-col bg-white dark:bg-gray-800 overflow-hidden"
      :class="
        windowed
          ? ''
          : 'max-w-6xl border border-gray-200 dark:border-gray-700 rounded-lg shadow-xl'
      "
    >
      <div
        class="flex items-center gap-3 px-4 py-2 border-b border-gray-200 dark:border-gray-700 shrink-0"
      >
        <span class="text-sm font-mono font-medium text-gray-800 dark:text-gray-100 truncate">
          {{ file }}
        </span>
        <span
          class="shrink-0 px-1.5 py-0.5 text-[10px] font-semibold rounded bg-blue-100 dark:bg-blue-900/30 text-blue-700 dark:text-blue-400"
        >
          {{ serviceName }}
        </span>
        <span
          v-if="info"
          class="shrink-0 text-xs text-gray-500 dark:text-gray-400"
        >
          {{ info.totalLines.toLocaleString() }} lines, {{ formatBytes(info.sizeBytes) }}
        </span>
        <button
          v-if="info && line"
          class="shrink-0 px-2 py-1 text-xs border border-gray-300 dark:border-gray-600 rounded text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200"
          @click="scrollToTargetLine"
        >
          Jump to line {{ line.toLocaleString() }}
        </button>
        <button
          v-if="!windowed"
          class="ml-auto shrink-0 px-2 py-1 text-xs border border-gray-300 dark:border-gray-600 rounded text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200"
          @click="openInWindow"
        >
          Open in window
        </button>
        <button
          class="shrink-0 px-2 py-1 text-xs border border-gray-300 dark:border-gray-600 rounded text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200"
          :class="windowed ? 'ml-auto' : ''"
          @click="emit('close')"
        >
          Close (Esc)
        </button>
      </div>

      <div
        class="flex items-center gap-2 px-4 py-1.5 border-b border-gray-200 dark:border-gray-700 shrink-0 text-xs"
      >
        <input
          ref="searchInput"
          v-model="query"
          type="text"
          spellcheck="false"
          placeholder="Search in file... (Ctrl+F)"
          class="w-72 px-2 py-1 font-mono rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900"
          @keydown.enter.exact.prevent="gotoMatch(current + 1)"
          @keydown.shift.enter.prevent="gotoMatch(current - 1)"
        >
        <button
          class="px-1.5 py-1 border rounded font-mono"
          :class="
            caseSensitive
              ? 'border-blue-400 dark:border-blue-600 bg-blue-100 dark:bg-blue-900/40 text-blue-700 dark:text-blue-300'
              : 'border-gray-300 dark:border-gray-600 text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200'
          "
          title="Match case"
          @click="caseSensitive = !caseSensitive"
        >
          Aa
        </button>
        <button
          class="px-1.5 py-1 border rounded font-mono"
          :class="
            isRegex
              ? 'border-blue-400 dark:border-blue-600 bg-blue-100 dark:bg-blue-900/40 text-blue-700 dark:text-blue-300'
              : 'border-gray-300 dark:border-gray-600 text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200'
          "
          title="Regular expression"
          @click="isRegex = !isRegex"
        >
          .*
        </button>
        <span
          class="w-28 text-gray-500 dark:text-gray-400 tabular-nums"
          :title="
            result && result.matches.length < result.total
              ? `Only the first ${result.matches.length.toLocaleString()} matches are listed`
              : ''
          "
        >
          <template v-if="searching">Searching...</template>
          <template v-else-if="result">
            {{ result.total === 0 ? 'No matches' : `${current + 1} / ${result.total.toLocaleString()}` }}
          </template>
        </span>
        <button
          class="px-2 py-1 border border-gray-300 dark:border-gray-600 rounded text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 disabled:opacity-40"
          :disabled="!result || result.matches.length === 0"
          title="Previous match (Shift+Enter)"
          @click="gotoMatch(current - 1)"
        >
          Prev
        </button>
        <button
          class="px-2 py-1 border border-gray-300 dark:border-gray-600 rounded text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 disabled:opacity-40"
          :disabled="!result || result.matches.length === 0"
          title="Next match (Enter)"
          @click="gotoMatch(current + 1)"
        >
          Next
        </button>
        <button
          class="ml-auto px-2 py-1 border rounded"
          :class="
            showMatches
              ? 'border-blue-400 dark:border-blue-600 bg-blue-100 dark:bg-blue-900/40 text-blue-700 dark:text-blue-300'
              : 'border-gray-300 dark:border-gray-600 text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200'
          "
          title="Toggle the match list pane"
          @click="showMatches = !showMatches"
        >
          Matches{{ result ? ` (${result.total.toLocaleString()})` : '' }}
        </button>
      </div>

      <div
        v-if="error || searchError"
        class="px-4 py-2 text-sm text-red-700 dark:text-red-400 bg-red-50 dark:bg-red-900/30 border-b border-red-200 dark:border-red-800 break-all shrink-0"
      >
        {{ error ?? searchError }}
      </div>

      <div
        v-if="loading"
        class="flex-1 flex items-center justify-center text-sm text-gray-400"
      >
        Decompressing file...
      </div>

      <div
        v-else
        ref="viewport"
        class="flex-1 overflow-auto bg-gray-50 dark:bg-gray-900/50"
        @scroll.passive="onScroll"
      >
        <div
          class="relative w-max min-w-full"
          :style="{ height: `${totalLines * ROW_HEIGHT}px` }"
        >
          <div
            class="absolute top-0 left-0 w-max min-w-full"
            :style="{ transform: `translateY(${range.start * ROW_HEIGHT}px)` }"
          >
            <div
              v-for="row in rows"
              :key="row.number"
              class="flex w-max min-w-full font-mono text-[11px] leading-[18px]"
              :class="
                row.number === currentLine
                  ? 'bg-orange-100 dark:bg-orange-900/40'
                  : row.number === line
                    ? 'bg-yellow-100 dark:bg-yellow-900/30'
                    : 'hover:bg-gray-100 dark:hover:bg-gray-800'
              "
              :style="{ height: `${ROW_HEIGHT}px` }"
            >
              <span
                class="sticky left-0 shrink-0 px-2 text-right select-none border-r text-gray-400 dark:text-gray-500"
                :class="
                  row.number === currentLine
                    ? 'bg-orange-100 dark:bg-orange-900/40 border-orange-300 dark:border-orange-700 text-orange-700 dark:text-orange-400'
                    : row.number === line
                      ? 'bg-yellow-100 dark:bg-yellow-900/30 border-yellow-300 dark:border-yellow-700 text-yellow-700 dark:text-yellow-500'
                      : 'bg-gray-50 dark:bg-gray-900 border-gray-200 dark:border-gray-700'
                "
                :style="{ width: gutterWidth }"
              >
                {{ row.number }}
              </span>
              <span class="whitespace-pre pl-3 pr-6 text-gray-700 dark:text-gray-300">
                <template
                  v-for="(part, i) in highlightParts(row.text ?? '')"
                  :key="i"
                ><span
                  v-if="part.slot !== null"
                  :class="slotClass(part.slot)"
                >{{ part.text }}</span><template v-else>{{ part.text }}</template></template>
              </span>
            </div>
          </div>
        </div>
      </div>

      <div
        v-if="showMatches && result && !loading"
        class="shrink-0 h-56 flex flex-col border-t-2 border-gray-300 dark:border-gray-600"
      >
        <div
          class="px-4 py-1 text-[10px] uppercase tracking-wide text-gray-400 dark:text-gray-500 border-b border-gray-200 dark:border-gray-700 shrink-0"
        >
          Matches
          <template v-if="result.matches.length < result.total">
            — showing first {{ result.matches.length.toLocaleString() }} of
            {{ result.total.toLocaleString() }}
          </template>
        </div>
        <div
          v-if="result.matches.length === 0"
          class="flex-1 flex items-center justify-center text-xs text-gray-400"
        >
          No matches
        </div>
        <div
          v-else
          ref="matchesViewport"
          class="flex-1 overflow-auto bg-gray-50 dark:bg-gray-900/50"
          @scroll.passive="onMatchesScroll"
        >
          <div
            class="relative w-max min-w-full"
            :style="{ height: `${result.matches.length * ROW_HEIGHT}px` }"
          >
            <div
              class="absolute top-0 left-0 w-max min-w-full"
              :style="{ transform: `translateY(${matchRange.start * ROW_HEIGHT}px)` }"
            >
              <div
                v-for="m in matchRows"
                :key="m.index"
                class="flex w-max min-w-full font-mono text-[11px] leading-[18px] cursor-pointer"
                :class="
                  m.index === current
                    ? 'bg-orange-100 dark:bg-orange-900/40'
                    : 'hover:bg-gray-100 dark:hover:bg-gray-800'
                "
                :style="{ height: `${ROW_HEIGHT}px` }"
                @click="gotoMatch(m.index)"
              >
                <span
                  class="sticky left-0 shrink-0 px-2 text-right select-none border-r"
                  :class="
                    m.index === current
                      ? 'bg-orange-100 dark:bg-orange-900/40 border-orange-300 dark:border-orange-700 text-orange-700 dark:text-orange-400'
                      : 'bg-gray-50 dark:bg-gray-900 border-gray-200 dark:border-gray-700 text-gray-400 dark:text-gray-500'
                  "
                  :style="{ width: gutterWidth }"
                >
                  {{ m.lineNumber }}
                </span>
                <span class="whitespace-pre pl-3 pr-6 text-gray-700 dark:text-gray-300">
                  <template
                    v-for="(part, i) in highlightParts(m.text)"
                    :key="i"
                  ><span
                    v-if="part.slot !== null"
                    :class="slotClass(part.slot)"
                  >{{ part.text }}</span><template v-else>{{ part.text }}</template></template>
                </span>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
