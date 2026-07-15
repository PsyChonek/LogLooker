<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import {
  compileQuery,
  isNegated,
  MAX_CRITERIA,
  type Combinator,
  type Criterion,
  type CriterionMode,
} from '@/utils/regexQuery';

// Builds a list of criteria and compiles them into one plain regex, which the
// parent puts into the query field. Each positive criterion is one named
// capture group, so its dot colour follows it into the highlighted results
// and the "Matched group" chart series.

const STORAGE_KEY = 'loglooker.queryBuilder';
const MODES: CriterionMode[] = ['contains', 'regex', 'notContains', 'notRegex'];

const emit = defineEmits<{ compiled: [query: string]; search: [] }>();

interface BuilderState {
  criteria: Criterion[];
  combinator: Combinator;
}

function emptyCriterion(): Criterion {
  return { mode: 'contains', text: '', label: '' };
}

function load(): BuilderState {
  try {
    const saved = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? '');
    if (Array.isArray(saved.criteria) && saved.criteria.length > 0) {
      return {
        criteria: saved.criteria.slice(0, MAX_CRITERIA).map(
          (c: Partial<Criterion>): Criterion => ({
            mode: MODES.includes(c.mode as CriterionMode) ? (c.mode as CriterionMode) : 'contains',
            text: typeof c.text === 'string' ? c.text : '',
            label: typeof c.label === 'string' ? c.label : '',
          }),
        ),
        combinator: saved.combinator === 'any' ? 'any' : 'all',
      };
    }
  } catch {
    // fall through to defaults
  }
  return { criteria: [emptyCriterion(), emptyCriterion()], combinator: 'all' };
}

const state = ref<BuilderState>(load());
const compiled = computed(() => compileQuery(state.value.criteria, state.value.combinator));

watch(
  state,
  () => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state.value));
    emit('compiled', compiled.value);
  },
  { deep: true },
);

// The parent's query field must show the compiled regex as soon as the
// builder opens, not only after the first edit
onMounted(() => emit('compiled', compiled.value));

// Palette slot of a row: its position among the criteria that compile to a
// capture group (positive, non-empty). Negations and empty rows carry none.
function slotOf(index: number): number | null {
  const criterion = state.value.criteria[index];
  if (isNegated(criterion.mode) || criterion.text === '') return null;
  let slot = 0;
  for (let i = 0; i <= index; i++) {
    const other = state.value.criteria[i];
    if (!isNegated(other.mode) && other.text !== '') slot++;
  }
  return slot;
}

function add() {
  if (state.value.criteria.length < MAX_CRITERIA) {
    state.value.criteria.push(emptyCriterion());
  }
}

function remove(index: number) {
  state.value.criteria.splice(index, 1);
  if (state.value.criteria.length === 0) {
    state.value.criteria.push(emptyCriterion());
  }
}

const CONTROL =
  'px-1.5 py-1 rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800';
const FIELD =
  'px-2 py-1 font-mono rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800';
</script>

<template>
  <div
    class="mb-4 p-3 rounded-md border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800/50 text-xs text-gray-600 dark:text-gray-400"
  >
    <div class="flex items-center gap-2 mb-2">
      <select
        v-model="state.combinator"
        :class="CONTROL"
      >
        <option value="all">
          All criteria must match
        </option>
        <option value="any">
          Any criterion matches
        </option>
      </select>
      <span class="text-gray-400">
        each line; "not" criteria always exclude. Colours carry over to results and charts.
      </span>
    </div>

    <div
      v-for="(criterion, i) in state.criteria"
      :key="i"
      class="flex items-center gap-2 mb-1.5"
    >
      <span
        class="w-2.5 h-2.5 rounded-sm shrink-0"
        :class="slotOf(i) === null ? 'border border-gray-300 dark:border-gray-600' : ''"
        :style="slotOf(i) !== null ? { backgroundColor: `var(--chart-${slotOf(i)})` } : {}"
        :title="slotOf(i) === null ? 'No colour — excluded or empty' : `Colour of this criterion`"
      />
      <select
        v-model="criterion.mode"
        :class="CONTROL"
      >
        <option value="contains">
          contains
        </option>
        <option value="regex">
          matches regex
        </option>
        <option value="notContains">
          does not contain
        </option>
        <option value="notRegex">
          does not match regex
        </option>
      </select>
      <input
        v-model="criterion.text"
        type="text"
        spellcheck="false"
        placeholder="text or pattern..."
        class="flex-1"
        :class="FIELD"
        @keydown.enter="emit('search')"
      >
      <input
        v-if="!isNegated(criterion.mode)"
        v-model="criterion.label"
        type="text"
        spellcheck="false"
        placeholder="name"
        title="Optional name for this criterion, used in highlights and chart legends"
        class="w-28"
        :class="FIELD"
        @keydown.enter="emit('search')"
      >
      <button
        class="px-2 py-1 border border-gray-300 dark:border-gray-600 rounded hover:text-gray-800 dark:hover:text-gray-200 disabled:opacity-50"
        :disabled="state.criteria.length === 1 && !criterion.text"
        @click="remove(i)"
      >
        Remove
      </button>
    </div>

    <button
      class="px-2 py-1 border border-gray-300 dark:border-gray-600 rounded hover:text-gray-800 dark:hover:text-gray-200 disabled:opacity-50"
      :disabled="state.criteria.length >= MAX_CRITERIA"
      @click="add"
    >
      Add criterion
    </button>
  </div>
</template>
