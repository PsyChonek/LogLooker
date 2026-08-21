<script setup lang="ts">
import { computed, onMounted, provide, ref, shallowRef, watch } from 'vue';
import {
  compileQuery,
  enabledCriteria,
  groupSlots,
  isNegated,
  paletteSlot,
  querySets,
  type CriterionMode,
  type CriterionNode,
  type GroupNode,
  type QuerySet,
  type QueryNode,
} from '@/utils/regexQuery';
import { builderOpenAction } from '@/utils/queryBuilderState';
import QueryGroup from './QueryGroup.vue';
import { builderContextKey } from './queryBuilderContext';

// Owns the builder tree: criteria in groups, nested as deep and as wide as the
// user cares to go, each group combining its children with AND/OR. The tree
// compiles into one plain regex, which the parent puts into the query field.
// Each positive criterion is one named capture group, so its dot colour follows
// it into the highlighted results and the "Matched group" chart series.

const STORAGE_KEY = 'loglooker.queryBuilder';
const MODES: CriterionMode[] = ['contains', 'regex', 'notContains', 'notRegex'];

// The compiled pattern alone does not say which capture groups belong to the
// same nested group, so the sets travel with it - that is what lets the results
// legend switch a whole set off for highlighting and export.
const emit = defineEmits<{ compiled: [query: string, sets: QuerySet[]]; search: [] }>();

// Whatever the query field holds when the builder opens. A pattern the builder
// itself wrote - a saved query, or one left in the field earlier - loads back
// in as its criteria, so opening the builder on a saved query shows that query
// rather than an empty form.
const props = defineProps<{ query: string }>();

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

// Rebuilds a saved tree defensively: unknown shapes are dropped, empty groups
// with them, so hand-edited storage cannot put the builder in a broken state
function sanitizeNode(raw: unknown): QueryNode | null {
  if (!raw || typeof raw !== 'object') return null;
  const node = raw as Partial<GroupNode> & Partial<CriterionNode>;
  if (node.type === 'group' && Array.isArray(node.children)) {
    const children = node.children
      .map(sanitizeNode)
      .filter((child): child is QueryNode => child !== null);
    if (children.length === 0) return null;
    return {
      type: 'group',
      combinator: node.combinator === 'any' ? 'any' : 'all',
      children,
      label: typeof node.label === 'string' ? node.label : '',
      disabled: node.disabled === true,
      collapsed: node.collapsed === true,
    };
  }
  return sanitizeCriterion(node);
}

function load(): GroupNode {
  try {
    const saved = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? '');
    if (saved.root) {
      const root = sanitizeNode(saved.root);
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
        children: saved.criteria.map(sanitizeCriterion),
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
// to a capture group (positive, non-empty), in tree order, wrapped around the
// palette so a ninth criterion repeats the first colour. Negations, empty rows
// and anything inside a muted group carry none.
const slots = computed(() => {
  const map = new Map<CriterionNode, number>();
  let position = 0;
  for (const criterion of enabledCriteria(root.value)) {
    if (!isNegated(criterion.mode) && criterion.text !== '') {
      map.set(criterion, paletteSlot(++position));
    }
  }
  return map;
});

// Group colours are positional, so muting a set leaves every other border
// where it was
const groupColors = computed(() => groupSlots(root.value));

// --- Drag to reorder and reparent ---
//
// A row or a whole group is dragged by its grip; the rows under the cursor
// report the insertion point they would take, and releasing anywhere over the
// builder applies it. Both refs hold nodes of the tree, so they are shallow -
// nothing here needs the deep tracking the tree itself gets.

const dragged = shallowRef<{ node: QueryNode; parent: GroupNode } | null>(null);
const dropTarget = shallowRef<{ parent: GroupNode; index: number } | null>(null);

// v-for keys, handed out per node and remembered for as long as the node lives
const nodeKeys = new WeakMap<QueryNode, number>();
let lastKey = 0;

function keyOf(node: QueryNode): number {
  let key = nodeKeys.get(node);
  if (key === undefined) nodeKeys.set(node, (key = ++lastKey));
  return key;
}

function contains(group: GroupNode, node: QueryNode): boolean {
  return group.children.some(
    (child) => child === node || (child.type === 'group' && contains(child, node)),
  );
}

/** The group holding `node`, or null for the root (which has none). */
function parentOf(node: QueryNode, group: GroupNode = root.value): GroupNode | null {
  for (const child of group.children) {
    if (child === node) return group;
    if (child.type === 'group') {
      const found = parentOf(node, child);
      if (found) return found;
    }
  }
  return null;
}

function hoverDrop(parent: GroupNode, index: number) {
  const drag = dragged.value;
  if (!drag) return;
  // A group cannot become its own descendant, and a node released back where
  // it already sits is not a move - both hide the indicator instead of
  // offering a drop that would do nothing or corrupt the tree
  const cycles =
    drag.node.type === 'group' && (drag.node === parent || contains(drag.node, parent));
  const at = parent === drag.parent ? parent.children.indexOf(drag.node) : -1;
  const noop = at >= 0 && (index === at || index === at + 1);
  dropTarget.value = cycles || noop ? null : { parent, index };
}

function endDrag() {
  dragged.value = null;
  dropTarget.value = null;
}

// A group emptied by the move goes with it, the way removing its last row
// does; the root instead keeps one blank criterion so the form stays usable
function pruneEmpty(group: GroupNode) {
  if (group.children.length > 0) return;
  if (group === root.value) {
    group.children.push(emptyCriterion());
    return;
  }
  const parent = parentOf(group);
  if (!parent) return;
  parent.children.splice(parent.children.indexOf(group), 1);
  pruneEmpty(parent);
}

function applyDrop() {
  const drag = dragged.value;
  const target = dropTarget.value;
  endDrag();
  if (!drag || !target) return;
  const from = drag.parent.children.indexOf(drag.node);
  if (from < 0) return;
  drag.parent.children.splice(from, 1);
  // Within one group, taking the node out ahead of the insertion point shifts
  // that point one slot left
  const index =
    target.parent === drag.parent && target.index > from ? target.index - 1 : target.index;
  target.parent.children.splice(index, 0, drag.node);
  pruneEmpty(drag.parent);
}

// The drop is taken on the builder as a whole, so releasing anywhere over it
// applies what the indicator shows
function onDragOver(event: DragEvent) {
  if (dropTarget.value) event.preventDefault();
}

provide(builderContextKey, {
  slotOf: (criterion) => slots.value.get(criterion) ?? null,
  groupSlotOf: (group) => groupColors.value.get(group) ?? null,
  search: () => emit('search'),
  keyOf,
  startDrag: (node, parent) => {
    dragged.value = { node, parent };
    dropTarget.value = null;
  },
  hoverDrop,
  dropAt: (parent, index) =>
    dropTarget.value !== null &&
    dropTarget.value.parent === parent &&
    dropTarget.value.index === index,
  isDragging: (node) => dragged.value?.node === node,
  endDrag,
});

watch(
  root,
  () => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ root: root.value }));
    emit('compiled', compiled.value, querySets(root.value));
  },
  { deep: true },
);

// Opening the builder reconciles it with the query field. A field holding a
// pattern this very tree compiles to is already ours, so the tree is kept whole
// - muted rows, set names and all, none of which a pattern carries. A different
// pattern that still came from a builder replaces the tree, which is what makes
// opening the builder on a saved query show that query. Anything else (a
// hand-written regex) leaves both alone, and the field keeps what it holds
// until a criterion gives the builder something to say - emitting an empty
// pattern here used to wipe the query the user had just loaded.
onMounted(() => {
  const action = builderOpenAction(root.value, props.query);
  if (action.kind === 'replace') {
    // The deep watch persists the tree and hands the recompiled pattern back
    root.value = action.root;
  }
});
</script>

<template>
  <div
    class="mb-4 p-3 rounded-md border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800/50 text-xs text-gray-600 dark:text-gray-400"
    @dragover="onDragOver"
    @drop.prevent="applyDrop"
  >
    <div class="mb-2 text-gray-400">
      Groups nest with AND/OR, e.g. (A or B) and (C and D); "not" criteria always exclude the whole
      entry - stack trace included - wherever they sit. Drag a row or a group by its grip to move it
      within its group or into another one, and fold a group with the arrow to get it out of the
      way. Colours carry over to results and charts.
    </div>
    <QueryGroup :group="root" :depth="0" />
  </div>
</template>
