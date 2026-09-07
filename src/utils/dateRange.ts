// Relative date presets. A preset is stored by id, never as the pair of dates it
// resolved to, so "Last 7 days" keeps meaning the last 7 days as the clock rolls
// past midnight or the app is reopened days later.

export type PresetId =
  | 'today'
  | 'yesterday'
  | 'last3'
  | 'last7'
  | 'last14'
  | 'last30'
  | 'thisMonth';

export interface DateRange {
  from: string;
  to: string;
}

export interface DateRangeSelection extends DateRange {
  preset: PresetId | null;
}

export interface DatePreset {
  id: PresetId;
  label: string;
  range: (today: Date) => DateRange;
}

export function toKey(date: Date): string {
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  return `${date.getFullYear()}-${month}-${day}`;
}

export function parseKey(key: string): Date {
  const [year, month, day] = key.split('-').map(Number);
  return new Date(year, month - 1, day);
}

export function todayKey(): string {
  return toKey(new Date());
}

function shift(today: Date, days: number): string {
  return toKey(new Date(today.getFullYear(), today.getMonth(), today.getDate() - days));
}

function trailing(days: number): (today: Date) => DateRange {
  return (today) => ({ from: shift(today, days - 1), to: toKey(today) });
}

export const DATE_PRESETS: DatePreset[] = [
  { id: 'today', label: 'Today', range: (today) => ({ from: toKey(today), to: toKey(today) }) },
  {
    id: 'yesterday',
    label: 'Yesterday',
    range: (today) => ({ from: shift(today, 1), to: shift(today, 1) }),
  },
  { id: 'last3', label: 'Last 3 days', range: trailing(3) },
  { id: 'last7', label: 'Last 7 days', range: trailing(7) },
  { id: 'last14', label: 'Last 14 days', range: trailing(14) },
  { id: 'last30', label: 'Last 30 days', range: trailing(30) },
  {
    id: 'thisMonth',
    label: 'This month',
    range: (today) => ({
      from: toKey(new Date(today.getFullYear(), today.getMonth(), 1)),
      to: toKey(today),
    }),
  },
];

export function findPreset(id: PresetId | null): DatePreset | null {
  if (!id) return null;
  return DATE_PRESETS.find((preset) => preset.id === id) ?? null;
}

// Resolves a preset against a day key ("2026-07-24") so callers can depend on a
// reactive "today" value and recompute when the day changes.
export function resolvePreset(id: PresetId, day: string): DateRange | null {
  return findPreset(id)?.range(parseKey(day)) ?? null;
}
