import { compileQuery, parseQuery, type GroupNode } from './regexQuery.ts';

export type BuilderOpenAction =
  | { kind: 'keep' }
  | { kind: 'replace'; root: GroupNode };

/**
 * Reconciles the persisted builder tree with the query field when the builder
 * opens. Only another builder-generated pattern may replace the tree. A plain,
 * hand-edited or cleared query remains authoritative and must not be replaced
 * by the older persisted builder pattern.
 */
export function builderOpenAction(
  storedRoot: GroupNode,
  currentQuery: string,
): BuilderOpenAction {
  if (compileQuery(storedRoot) === currentQuery) return { kind: 'keep' };
  const root = parseQuery(currentQuery);
  return root ? { kind: 'replace', root } : { kind: 'keep' };
}
