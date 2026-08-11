<script setup lang="ts">
import { computed, onMounted, provide, ref, watch } from 'vue';
import {
  compileQuery,
  flatCriteria,
  isNegated,
  MAX_CRITERIA,
  MAX_DEPTH,
  type CriterionMode,
  type CriterionNode,
  type GroupNode,
  type QueryNode,
} from '@/utils/regexQuery';
import QueryGroup from './QueryGroup.vue';
import { builderContextKey } from './queryBuilderContext';

// Owns the builder tree: groups of criteria nested up to MAX_DEPTH, each
// group combining its children with AND/OR. The tree compiles into one plain
// regex, which the parent puts into the query field. Each positive criterion
// is one named capture group, so its dot colour follows it into the
// highlighted results and the "Matched group" chart series.

const STORAGE_KEY = 'loglooker.queryBuilder';
const MODES: CriterionMode[] = ['contains', 'regex', 'notContains', 'notRegex'];

const emit = defineEmits<{ compiled: [query: string]; search: [] }>();

function emptyCriterion(): CriterionNode {
  return { type: 'criterion', mode: 'contains', text: '', label: '' };
}

function defaultRoot(): GroupNode {
  return { type: 'group', combinator: 'all', children: [emptyCriterion(), emptyCriterion()] };
}

function sanitizeCriterion(raw: Partial<CriterionNode>): CriterionNode {
  return {
    type: 'criterion',
    mode: MODES.includes(raw.mode as CriterionMode) ? (raw.mode as CriterionMode) : 'contains',
    text: typeof raw.text === 'string' ? raw.text : '',
    label: typeof raw.label === 'string' ? raw.label : '',
    disabled: raw.disabled === true,
  };
}

// Rebuilds a saved tree defensively: unknown shapes are dropped, the depth
// and criterion caps are enforced even against hand-edited storage
function sanitizeNode(raw: unknown, budget: { left: number }, depth: number): QueryNode | null {
  if (!raw || typeof raw !== 'object') return null;
  const node = raw as Partial<GroupNode> & Partial<CriterionNode>;
  if (node.type === 'group' && Array.isArray(node.children)) {
    if (depth >= MAX_DEPTH) return null;
    const children = node.children
      .map((child) => sanitizeNode(child, budget, depth + 1))
      .filter((child): child is QueryNode => child !== null);
    if (children.length === 0) return null;
    return { type: 'group', combinator: node.combinator === 'any' ? 'any' : 'all', children };
  }
  if (budget.left <= 0) return null;
  budget.left--;
  return sanitizeCriterion(node);
}

function load(): GroupNode {
  try {
    const saved = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? '');
    const budget = { left: MAX_CRITERIA };
    if (saved.root) {
      const root = sanitizeNode(saved.root, budget, 0);
      if (root) {
        return root.type === 'group'
          ? root
          : { type: 'group', combinator: 'all', children: [root] };
      }
    }
    // Older versions stored a flat criterion list with one combinator
    if (Array.isArray(saved.criteria) && saved.criteria.length > 0) {
      return {
        type: 'group',
        combinator: saved.combinator === 'any' ? 'any' : 'all',
        children: saved.criteria.slice(0, MAX_CRITERIA).map(sanitizeCriterion),
      };
    }
  } catch {
    // fall through to defaults
  }
  return defaultRoot();
}

const root = ref<GroupNode>(load());
const compiled = computed(() => compileQuery(root.value));

// Palette slot of a criterion: its position among the criteria that compile
// to a capture group (positive, non-empty), in tree order. Negations and
// empty rows carry none.
const slots = computed(() => {
  const map = new Map<CriterionNode, number>();
  let slot = 0;
  for (const criterion of flatCriteria(root.value)) {
    if (!isNegated(criterion.mode) && criterion.text !== '' && !criterion.disabled)
      map.set(criterion, ++slot);
  }
  return map;
});

provide(builderContextKey, {
  slotOf: (criterion) => slots.value.get(criterion) ?? null,
  canAdd: () => flatCriteria(root.value).length < MAX_CRITERIA,
  search: () => emit('search'),
});

watch(
  root,
  () => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ root: root.value }));
    emit('compiled', compiled.value);
  },
  { deep: true },
);

// The parent's query field must show the compiled regex as soon as the
// builder opens, not only after the first edit
onMounted(() => emit('compiled', compiled.value));
</script>

<template>
  <div
    class="mb-4 p-3 rounded-md border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800/50 text-xs text-gray-600 dark:text-gray-400"
  >
    <div class="mb-2 text-gray-400">
      Groups nest with AND/OR, e.g. (A or B) and (C and D); "not" criteria always exclude the line,
      wherever they sit. Colours carry over to results and charts.
    </div>
    <QueryGroup :group="root" :depth="0" />
  </div>
</template>
