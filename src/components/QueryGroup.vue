<script setup lang="ts">
/* eslint-disable vue/no-mutating-props -- the tree is owned by QueryBuilder, edited in place here */
import { computed, inject } from 'vue';
import BaseSelect, { type SelectOption } from '@/components/BaseSelect.vue';
import { isNegated, type CriterionNode, type GroupNode, type QueryNode } from '@/utils/regexQuery';
import { builderContextKey } from './queryBuilderContext';

// Recursive editor for one group of the query tree. The tree is owned by
// QueryBuilder, which persists and compiles it; this component mutates the
// nodes in place, hence the vue/no-mutating-props disable in the template.

const props = defineProps<{
  group: GroupNode;
  // Absent on the root group, which cannot be removed, muted or coloured -
  // it is the query itself
  parent?: GroupNode;
  depth: number;
  // An ancestor group is muted, so nothing in here takes part in the query
  muted?: boolean;
}>();

const ctx = inject(builderContextKey)!;

// Muted either directly or by an ancestor: the rows stay editable to read and
// tweak, but everything is dimmed and out of the compiled query
const inactive = computed(() => props.muted === true || props.group.disabled === true);

const slot = computed(() => ctx.groupSlotOf(props.group));

function emptyCriterion(): CriterionNode {
  return { type: 'criterion', mode: 'contains', text: '', label: '' };
}

function addCriterion() {
  props.group.children.push(emptyCriterion());
}

// A new group starts with the opposite combinator, since (A OR B) AND (...)
// is the usual reason to nest at all
function addGroup() {
  props.group.children.push({
    type: 'group',
    combinator: props.group.combinator === 'all' ? 'any' : 'all',
    children: [emptyCriterion()],
    label: '',
  });
}

function removeChild(index: number) {
  props.group.children.splice(index, 1);
  if (props.group.children.length > 0) return;
  if (props.parent) {
    props.parent.children.splice(props.parent.children.indexOf(props.group), 1);
  } else {
    props.group.children.push(emptyCriterion());
  }
}

function removeSelf() {
  const parent = props.parent;
  if (!parent) return;
  parent.children.splice(parent.children.indexOf(props.group), 1);
  if (parent.children.length === 0) parent.children.push(emptyCriterion());
}

// --- Drag to reorder ---
//
// Only the grip starts a drag, so text in the inputs stays selectable. The
// insertion point comes from whichever row the cursor is over: its top half
// inserts before it, its bottom half after it. QueryBuilder validates the point
// and takes the drop.

function beginDrag(node: QueryNode, parent: GroupNode, event: DragEvent) {
  ctx.startDrag(node, parent);
  if (!event.dataTransfer) return;
  event.dataTransfer.effectAllowed = 'move';
  // WebView2 starts no drag at all without a payload
  event.dataTransfer.setData('text/plain', 'query-node');
  // Drag the whole row rather than the grip alone
  const row = (event.currentTarget as HTMLElement).closest('[data-drag-row]');
  if (row) event.dataTransfer.setDragImage(row, 16, 12);
}

function sideOf(event: DragEvent): number {
  const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
  return event.clientY >= rect.top + rect.height / 2 ? 1 : 0;
}

function overChild(index: number, event: DragEvent) {
  ctx.hoverDrop(props.group, index + sideOf(event));
}

// This group's own header takes the drop above the group, in its parent, or -
// on the lower half - as the group's first child. Both put the insertion line
// right where the cursor is; landing after the whole group is the top half of
// whatever follows it, or that parent's append zone.
function overSelf(event: DragEvent) {
  const parent = props.parent;
  if (!parent) return;
  if (sideOf(event) === 1) ctx.hoverDrop(props.group, 0);
  else ctx.hoverDrop(parent, parent.children.indexOf(props.group));
}

// The button row under the last child appends to this group - the way a node
// moves into a group whose rows the cursor never passes over
function overEnd() {
  ctx.hoverDrop(props.group, props.group.children.length);
}

const COMBINATOR_OPTIONS: SelectOption<GroupNode['combinator']>[] = [
  { value: 'all', label: 'ALL of the following' },
  { value: 'any', label: 'ANY of the following' },
];

const MODE_OPTIONS: SelectOption<CriterionNode['mode']>[] = [
  { value: 'contains', label: 'contains' },
  { value: 'regex', label: 'matches regex' },
  { value: 'notContains', label: 'does not contain' },
  { value: 'notRegex', label: 'does not match regex' },
];

const FIELD =
  'px-2 py-1 font-mono rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800';
const BUTTON =
  'px-2 py-1 border border-gray-300 dark:border-gray-600 rounded hover:text-gray-800 dark:hover:text-gray-200 disabled:opacity-50';
const GRIP =
  'shrink-0 cursor-grab active:cursor-grabbing text-gray-300 dark:text-gray-600 hover:text-gray-500 dark:hover:text-gray-400';
// Where the dragged node would land, drawn in the gap between two rows
const DROP_LINE = 'h-0.5 -mt-1 mb-1 rounded-full bg-blue-500 dark:bg-blue-400';
</script>

<template>
  <!-- eslint-disable vue/no-mutating-props -- the tree is owned by QueryBuilder, edited in place here -->
  <div
    :data-drag-row="parent ? '' : null"
    :class="[
      depth > 0
        ? 'mb-1.5 p-2 rounded border border-l-4 border-gray-300 dark:border-gray-600 bg-white/40 dark:bg-gray-900/30'
        : '',
      group.disabled ? 'opacity-50' : '',
      ctx.isDragging(group) ? 'opacity-40' : '',
    ]"
    :style="depth > 0 && slot !== null ? { borderLeftColor: `var(--chart-${slot})` } : {}"
  >
    <div class="flex items-center gap-2 mb-1.5" @dragover="overSelf">
      <template v-if="parent">
        <!-- Dragging the header moves the whole group, subgroups included -->
        <span
          :class="GRIP"
          draggable="true"
          title="Drag to move this group"
          @dragstart="beginDrag(group, parent, $event)"
          @dragend="ctx.endDrag()"
        >
          <svg class="size-3 block" viewBox="0 0 8 12" aria-hidden="true">
            <path
              d="M1.5 3h5M1.5 6h5M1.5 9h5"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
            />
          </svg>
        </span>
        <input
          type="checkbox"
          class="shrink-0 cursor-pointer"
          :checked="!group.disabled"
          :title="
            group.disabled
              ? 'Group disabled - click to put its criteria back into the query'
              : 'Disable this whole group'
          "
          @change="group.disabled = !($event.target as HTMLInputElement).checked"
        />
        <!-- A ring, where a criterion wears a filled square: a group shares the
             eight-colour palette with the criteria, so the shape is what tells
             "this whole set" from "this one row" when the colours coincide -->
        <span
          class="w-2.5 h-2.5 rounded-full border-2 shrink-0"
          :style="slot !== null ? { borderColor: `var(--chart-${slot})` } : {}"
          title="Colour of this group"
        />
      </template>
      <BaseSelect v-model="group.combinator" :options="COMBINATOR_OPTIONS" :disabled="inactive" />
      <input
        v-if="parent"
        v-model="group.label"
        type="text"
        spellcheck="false"
        placeholder="group name"
        title="Optional name for this group, used in the results legend"
        class="w-28"
        :class="FIELD"
        :disabled="inactive"
        @keydown.enter="ctx.search()"
      />
      <button v-if="parent" :class="BUTTON" @click="removeSelf">Remove group</button>
    </div>

    <template v-for="(child, i) in group.children" :key="ctx.keyOf(child)">
      <div v-if="ctx.dropAt(group, i)" :class="DROP_LINE" />
      <QueryGroup
        v-if="child.type === 'group'"
        :group="child"
        :parent="group"
        :depth="depth + 1"
        :muted="inactive"
      />
      <div
        v-else
        data-drag-row
        class="flex items-center gap-2 mb-1.5"
        :class="[child.disabled ? 'opacity-50' : '', ctx.isDragging(child) ? 'opacity-40' : '']"
        @dragover="overChild(i, $event)"
      >
        <span
          :class="GRIP"
          draggable="true"
          title="Drag to move this criterion"
          @dragstart="beginDrag(child, group, $event)"
          @dragend="ctx.endDrag()"
        >
          <svg class="size-3 block" viewBox="0 0 8 12" aria-hidden="true">
            <path
              d="M1.5 3h5M1.5 6h5M1.5 9h5"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
            />
          </svg>
        </span>
        <input
          type="checkbox"
          class="shrink-0 cursor-pointer"
          :checked="!child.disabled"
          :disabled="inactive"
          :title="
            child.disabled ? 'Criterion disabled - click to enable' : 'Disable this criterion'
          "
          @change="child.disabled = !($event.target as HTMLInputElement).checked"
        />
        <span
          class="w-2.5 h-2.5 rounded-sm shrink-0"
          :class="ctx.slotOf(child) === null ? 'border border-gray-300 dark:border-gray-600' : ''"
          :style="
            ctx.slotOf(child) !== null
              ? { backgroundColor: `var(--chart-${ctx.slotOf(child)})` }
              : {}
          "
          :title="
            ctx.slotOf(child) === null
              ? 'No colour - excluded or empty'
              : 'Colour of this criterion'
          "
        />
        <BaseSelect
          v-model="child.mode"
          :options="MODE_OPTIONS"
          :disabled="child.disabled || inactive"
        />
        <input
          v-model="child.text"
          type="text"
          spellcheck="false"
          placeholder="text or pattern..."
          class="flex-1"
          :class="FIELD"
          :disabled="child.disabled || inactive"
          @keydown.enter="ctx.search()"
        />
        <input
          v-if="!isNegated(child.mode)"
          v-model="child.label"
          type="text"
          spellcheck="false"
          placeholder="name"
          title="Optional name for this criterion, used in highlights and chart legends"
          class="w-28"
          :class="FIELD"
          :disabled="child.disabled || inactive"
          @keydown.enter="ctx.search()"
        />
        <button
          :class="BUTTON"
          :disabled="!parent && group.children.length === 1 && !child.text"
          @click="removeChild(i)"
        >
          Remove
        </button>
      </div>
    </template>

    <div v-if="ctx.dropAt(group, group.children.length)" :class="DROP_LINE" />

    <!-- Also the drop zone that appends to this group, which is how a row gets
         into a group it is not already hovering a child of -->
    <div class="flex items-center gap-2" @dragover="overEnd">
      <button :class="BUTTON" :disabled="inactive" @click="addCriterion">Add criterion</button>
      <button :class="BUTTON" :disabled="inactive" @click="addGroup">Add group</button>
    </div>
  </div>
</template>
