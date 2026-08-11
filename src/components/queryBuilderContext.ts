import type { InjectionKey } from 'vue';
import type { CriterionNode } from '@/utils/regexQuery';

// Contract between QueryBuilder (owns the tree) and the recursive QueryGroup
// rows: colour slots, the shared criterion cap, and Enter-to-search.
export interface BuilderContext {
  slotOf: (criterion: CriterionNode) => number | null;
  canAdd: () => boolean;
  search: () => void;
}

export const builderContextKey: InjectionKey<BuilderContext> = Symbol('queryBuilder');
