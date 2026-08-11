import type { InjectionKey } from 'vue';
import type { CriterionNode, GroupNode, QueryNode } from '@/utils/regexQuery';

// Contract between QueryBuilder (owns the tree) and the recursive QueryGroup
// rows: colour slots, Enter-to-search, and the drag that moves a criterion or a
// whole group to another slot - under a different parent if that is where it
// was dropped. The drag lives in QueryBuilder because it crosses component
// boundaries: the row being dragged and the group it lands in are usually two
// different QueryGroup instances.
export interface BuilderContext {
  slotOf: (criterion: CriterionNode) => number | null;
  groupSlotOf: (group: GroupNode) => number | null;
  search: () => void;
  // Stable identity per node, so the v-for keys survive a reorder: index keys
  // would leave the DOM in place and shuffle the values between the inputs
  keyOf: (node: QueryNode) => number;
  startDrag: (node: QueryNode, parent: GroupNode) => void;
  // Insertion point under the cursor: the dragged node would land at `index`
  // among `parent`'s children. Refused - and the indicator hidden - when the
  // move would change nothing or would put a group inside itself.
  hoverDrop: (parent: GroupNode, index: number) => void;
  // True while this exact insertion point is the pending one, which is what
  // paints the line between two rows
  dropAt: (parent: GroupNode, index: number) => boolean;
  isDragging: (node: QueryNode) => boolean;
  endDrag: () => void;
}

export const builderContextKey: InjectionKey<BuilderContext> = Symbol('queryBuilder');
