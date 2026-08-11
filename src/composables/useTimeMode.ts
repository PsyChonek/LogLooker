import { ref, watchEffect } from 'vue';

/** Which clock log times are read on: the logs' own (UTC), or this machine's. */
export type TimeMode = 'utc' | 'local';

const STORAGE_KEY = 'loglooker.timeMode';

const mode = ref<TimeMode>(localStorage.getItem(STORAGE_KEY) === 'local' ? 'local' : 'utc');

watchEffect(() => localStorage.setItem(STORAGE_KEY, mode.value));

export function useTimeMode() {
  return { mode };
}

/**
 * Interprets a picker wall time on the clock currently selected in the UI and
 * returns the same instant as a naive UTC value understood by the backend.
 */
export function pickerTimeToUtc(value: string, endOfMinute = false): string | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(value);
  if (!match) return null;
  const [, year, month, day, hour, minute] = match.map(Number);
  const millis =
    mode.value === 'utc'
      ? Date.UTC(year, month - 1, day, hour, minute, endOfMinute ? 59 : 0)
      : new Date(year, month - 1, day, hour, minute, endOfMinute ? 59 : 0).getTime();
  const utc = new Date(millis).toISOString().slice(0, 19);
  return endOfMinute ? `${utc}.999999999` : utc;
}

/** Keeps a selected instant stable when the user changes the displayed clock. */
export function convertPickerTime(value: string, from: TimeMode, to: TimeMode): string {
  if (!value || from === to) return value;
  const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(value);
  if (!match) return value;
  const [, year, month, day, hour, minute] = match.map(Number);
  const instant =
    from === 'utc'
      ? new Date(Date.UTC(year, month - 1, day, hour, minute))
      : new Date(year, month - 1, day, hour, minute);
  const rendered = render(instant, to === 'utc', false, 'T');
  return rendered.slice(0, 16);
}

/** `UTC`, `UTC+2`, `UTC-3:30` - how far ahead of UTC a clock runs. */
export function zoneLabel(offsetMinutes: number): string {
  if (offsetMinutes === 0) return 'UTC';
  const sign = offsetMinutes > 0 ? '+' : '-';
  const hours = Math.floor(Math.abs(offsetMinutes) / 60);
  const minutes = Math.abs(offsetMinutes) % 60;
  const fraction = minutes ? `:${String(minutes).padStart(2, '0')}` : '';
  return `UTC${sign}${hours}${fraction}`;
}

function render(instant: Date, utc: boolean, withMillis: boolean, separator: string): string {
  const pad = (value: number, width = 2) => String(value).padStart(width, '0');
  const [year, month, day, hour, minute, second, millis] = utc
    ? [
        instant.getUTCFullYear(),
        instant.getUTCMonth() + 1,
        instant.getUTCDate(),
        instant.getUTCHours(),
        instant.getUTCMinutes(),
        instant.getUTCSeconds(),
        instant.getUTCMilliseconds(),
      ]
    : [
        instant.getFullYear(),
        instant.getMonth() + 1,
        instant.getDate(),
        instant.getHours(),
        instant.getMinutes(),
        instant.getSeconds(),
        instant.getMilliseconds(),
      ];
  const text = `${year}-${pad(month)}-${pad(day)}${separator}${pad(hour)}:${pad(minute)}:${pad(second)}`;
  return withMillis ? `${text}.${pad(millis, 3)}` : text;
}

/**
 * A log timestamp is a wall-clock reading with no zone in it, so the instant it names
 * is only known once the service's log clock is - `logOffsetMinutes`, measured in
 * cache.rs. Without that offset there is nothing to convert against and the reading is
 * shown as it stands, unlabelled: better a bare time than one labelled with a guess.
 */
export function formatLogTime(
  timestamp: string | null | undefined,
  offsetMinutes: number | null,
  separator = ' ',
): { text: string; zone: string } {
  if (!timestamp) return { text: '', zone: '' };
  const raw = timestamp.replace('T', separator);
  const parsed = offsetMinutes === null ? NaN : Date.parse(`${timestamp}Z`);
  if (Number.isNaN(parsed)) return { text: raw, zone: '' };

  const instant = new Date(parsed - (offsetMinutes as number) * 60_000);
  const utc = mode.value === 'utc';
  return {
    text: render(instant, utc, timestamp.includes('.'), separator),
    // getTimezoneOffset counts the other way round, and answers for the instant it is
    // asked about - so a July timestamp keeps its summer offset when read in January
    zone: zoneLabel(utc ? 0 : -instant.getTimezoneOffset()),
  };
}

/** Moves a naive `2026-07-14T13:05:00` chart label onto the displayed clock. */
export function shiftLogLabel(label: string, offsetMinutes: number | null): string {
  return formatLogTime(label, offsetMinutes, 'T').text;
}

/** The zone times are being read in, for a heading. `null` while the log clock is unknown. */
export function displayZone(offsetMinutes: number | null): string | null {
  if (offsetMinutes === null) return null;
  return mode.value === 'utc' ? 'UTC' : zoneLabel(-new Date().getTimezoneOffset());
}
