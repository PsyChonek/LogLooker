<script setup lang="ts">
/* eslint-disable vue/no-mutating-props -- the tree is owned by QueryBuilder, edited in place here */
import { inject } from 'vue';
import BaseSelect, { type SelectOption } from '@/components/BaseSelect.vue';
import { isNegated, MAX_DEPTH, type CriterionNode, type GroupNode } from '@/utils/regexQuery';
import { builderContextKey } from './queryBuilderContext';

// Recursive editor for one group of the query tree. The tree is owned by
// QueryBuilder, which persists and compiles it; this component mutates the
// nodes in place, hence the vue/no-mutating-props disable in the template.

const props = defineProps<{
  group: GroupNode;
  // Absent on the root group, which cannot be removed
  parent?: GroupNode;
  depth: number;
}>();

const ctx = inject(builderContextKey)!;

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
</script>

<template>
  <!-- eslint-disable vue/no-mutating-props -- the tree is owned by QueryBuilder, edited in place here -->
  <div
    :class="
      depth > 0
        ? 'mb-1.5 p-2 rounded border border-gray-300 dark:border-gray-600 bg-white/40 dark:bg-gray-900/30'
        : ''
    "
  >
    <div class="flex items-center gap-2 mb-1.5">
      <BaseSelect v-model="group.combinator" :options="COMBINATOR_OPTIONS" />
      <button v-if="parent" :class="BUTTON" @click="removeSelf">Remove group</button>
    </div>

    <template v-for="(child, i) in group.children" :key="i">
      <QueryGroup v-if="child.type === 'group'" :group="child" :parent="group" :depth="depth + 1" />
      <div
        v-else
        class="flex items-center gap-2 mb-1.5"
        :class="child.disabled ? 'opacity-50' : ''"
      >
        <input
          type="checkbox"
          class="shrink-0 cursor-pointer"
          :checked="!child.disabled"
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
        <BaseSelect v-model="child.mode" :options="MODE_OPTIONS" :disabled="child.disabled" />
        <input
          v-model="child.text"
          type="text"
          spellcheck="false"
          placeholder="text or pattern..."
          class="flex-1"
          :class="FIELD"
          :disabled="child.disabled"
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
          :disabled="child.disabled"
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

    <div class="flex items-center gap-2">
      <button :class="BUTTON" :disabled="!ctx.canAdd()" @click="addCriterion">Add criterion</button>
      <button
        v-if="depth < MAX_DEPTH - 1"
        :class="BUTTON"
        :disabled="!ctx.canAdd()"
        @click="addGroup"
      >
        Add group
      </button>
    </div>
  </div>
</template>
