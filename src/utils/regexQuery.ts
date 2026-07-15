// The query builder's criteria, their compilation into one plain regex, and
// the per-group highlighting shared by the results table and the raw viewer.
//
// Each positive criterion becomes a named capture group, so one colour slot
// follows it from the builder row through the highlighted hits to the chart
// series ("Group by: Matched group"). ALL and NOT criteria compile to
// look-arounds; the backend falls back to its backtracking engine for those.

export type CriterionMode = 'contains' | 'regex' | 'notContains' | 'notRegex';

export interface Criterion {
  mode: CriterionMode;
  text: string;
  // Optional group name; empty falls back to g1, g2, ...
  label: string;
}

export type Combinator = 'all' | 'any';

// The chart palette has eight slots and never cycles; the builder keeps the
// criterion count inside them so every criterion holds a colour of its own
export const MAX_CRITERIA = 8;

const ESCAPE = /[.*+?^${}()|[\]\\]/g;

export function isNegated(mode: CriterionMode): boolean {
  return mode === 'notContains' || mode === 'notRegex';
}

function body(criterion: Criterion): string {
  return criterion.mode === 'regex' || criterion.mode === 'notRegex'
    ? criterion.text
    : criterion.text.replace(ESCAPE, '\\$&');
}

// Group names must be valid regex identifiers and unique within the pattern
export function positiveGroupNames(positives: Criterion[]): string[] {
  const names: string[] = [];
  positives.forEach((criterion, i) => {
    let name = criterion.label.replace(/[^A-Za-z0-9_]+/g, '_').replace(/^_+|_+$/g, '');
    if (name && /^[0-9]/.test(name)) name = `g_${name}`;
    if (!name) name = `g${i + 1}`;
    while (names.includes(name)) name += '_';
    names.push(name);
  });
  return names;
}

/**
 * Compiles the criteria into a single plain regex.
 *
 * - ANY without NOTs: `(?<a>A)|(?<b>B)` — plain alternation, fast engine.
 * - ALL: `^(?=.*(?<a>A))(?=.*(?<b>B))` — one lookahead per criterion.
 * - ANY with NOTs: each positive in a lookahead with an empty alternative
 *   (`(?=.*(?<a>A)|)` always succeeds, but captures when present) behind an
 *   "at least one occurs" assertion, so every group that occurs keeps its
 *   colour instead of only the leftmost one.
 */
export function compileQuery(criteria: Criterion[], combinator: Combinator): string {
  const used = criteria.filter((criterion) => criterion.text !== '');
  const positives = used.filter((criterion) => !isNegated(criterion.mode));
  const negatives = used.filter((criterion) => isNegated(criterion.mode));
  const names = positiveGroupNames(positives);
  const groups = positives.map((criterion, i) => `(?<${names[i]}>${body(criterion)})`);
  const nots = negatives.map((criterion) => `(?!.*${body(criterion)})`);

  if (groups.length === 0) {
    return nots.length === 0 ? '' : `^${nots.join('')}`;
  }
  if (groups.length === 1 && nots.length === 0) {
    return groups[0];
  }
  if (combinator === 'all' || groups.length === 1) {
    return `^${groups.map((group) => `(?=.*${group})`).join('')}${nots.join('')}`;
  }
  if (nots.length === 0) {
    return groups.join('|');
  }
  const anyOf = `(?=.*(?:${positives.map(body).join('|')}))`;
  return `^${anyOf}${nots.join('')}${groups.map((group) => `(?=.*${group}|)`).join('')}`;
}

// --- Highlighting ---

export interface HighlightPart {
  text: string;
  // null: plain text; 0: match without a group; 1-based colour slot otherwise
  slot: number | null;
}

/** Named capture groups in pattern order, read from the pattern text itself.
 * Look-behinds do not match the name charset, so they are skipped. */
export function namedGroupsOf(pattern: string): string[] {
  const names: string[] = [];
  const finder = /\(\?<([A-Za-z_][A-Za-z0-9_]*)>/g;
  let match: RegExpExecArray | null;
  while ((match = finder.exec(pattern))) names.push(match[1]);
  return names;
}

export function slotClass(slot: number | null): string {
  if (slot === null) return '';
  if (slot === 0) return 'hl-match';
  return `hl-${((slot - 1) % 8) + 1}`;
}

export interface Highlighter {
  parts(text: string): HighlightPart[];
}

const PLAIN = (text: string): HighlightPart[] => [{ text, slot: null }];

/** Splits lines into plain and matched segments, coloured by capture group.
 * Returns null when the query is empty or uses syntax JS regex lacks — the
 * match list still works then and only the inline highlighting is skipped. */
export function buildHighlighter(
  query: string,
  isRegex: boolean,
  caseSensitive: boolean,
): Highlighter | null {
  if (!query) return null;
  const source = isRegex ? query : query.replace(ESCAPE, '\\$&');
  let regex: RegExp;
  try {
    // 'd' provides per-group match indices
    regex = new RegExp(source, caseSensitive ? 'dg' : 'dgi');
  } catch {
    return null;
  }
  const slots = new Map(namedGroupsOf(source).map((name, i) => [name, i + 1]));
  // The builder's ALL/NOT forms are anchored and match at most once; running
  // their zero-width match at every position would find nothing new
  const anchored = source.startsWith('^');

  function parts(text: string): HighlightPart[] {
    if (!text) return PLAIN(text);
    const spans: { start: number; end: number; slot: number }[] = [];
    regex.lastIndex = 0;
    let match: RegExpExecArray | null;
    let rounds = 0;
    // Capped so a megabyte SQL body cannot stall rendering
    while (rounds < 200 && (match = regex.exec(text))) {
      rounds++;
      let groupSpans = 0;
      for (const [name, span] of Object.entries(match.indices?.groups ?? {})) {
        if (!span || span[1] <= span[0]) continue;
        spans.push({ start: span[0], end: span[1], slot: slots.get(name) ?? 0 });
        groupSpans++;
      }
      if (groupSpans === 0 && match[0].length > 0) {
        spans.push({ start: match.index, end: match.index + match[0].length, slot: 0 });
      }
      if (anchored) break;
      if (match[0].length === 0) regex.lastIndex++;
    }
    if (spans.length === 0) return PLAIN(text);

    // Earliest-first; on ties the longer span wins, so nested groups keep the
    // outer colour and later overlaps are dropped
    spans.sort((a, b) => a.start - b.start || b.end - a.end);
    const result: HighlightPart[] = [];
    let at = 0;
    for (const span of spans) {
      if (span.start < at) continue;
      if (span.start > at) result.push({ text: text.slice(at, span.start), slot: null });
      result.push({ text: text.slice(span.start, span.end), slot: span.slot });
      at = span.end;
    }
    if (at < text.length) result.push({ text: text.slice(at), slot: null });
    return result;
  }

  return { parts };
}
