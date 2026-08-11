import type { SortState } from '@/components/DataTable.vue';

export type SortAccessor<TRow> = (row: TRow) => string | number | null | undefined;

// Returns a sorted copy. Null/undefined values sort last regardless of
// direction; numbers compare numerically, everything else via localeCompare.
export function sortRows<TRow>(
  rows: readonly TRow[],
  sort: SortState | null,
  accessors: Record<string, SortAccessor<TRow>>,
): TRow[] {
  const get = sort ? accessors[sort.key] : undefined;
  if (!sort || !get) return [...rows];
  const dir = sort.ascending ? 1 : -1;
  return [...rows].sort((a, b) => {
    const va = get(a);
    const vb = get(b);
    if (va == null && vb == null) return 0;
    if (va == null) return 1;
    if (vb == null) return -1;
    const cmp =
      typeof va === 'number' && typeof vb === 'number'
        ? va - vb
        : String(va).localeCompare(String(vb));
    return cmp * dir;
  });
}
