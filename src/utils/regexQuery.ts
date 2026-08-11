// The query builder's criteria, their compilation into one plain regex, and
// the per-group highlighting shared by the results table and the raw viewer.
//
// Each positive criterion becomes a named capture group, so one colour slot
// follows it from the builder row through the highlighted hits to the chart
// series ("Group by: Matched group"). The builder nests criteria in AND/OR
// groups; anything beyond a flat OR compiles to look-arounds, for which the
// backend falls back to its backtracking engine.

export type CriterionMode = 'contains' | 'regex' | 'notContains' | 'notRegex';

export interface Criterion {
  mode: CriterionMode;
  text: string;
  // Optional group name; empty falls back to g1, g2, ...
  label: string;
  // Temporarily muted in the builder: kept in the tree and storage, but left
  // out of the compiled regex, colour slots and highlighting until re-enabled
  disabled?: boolean;
}

export type Combinator = 'all' | 'any';

export interface CriterionNode extends Criterion {
  type: 'criterion';
}

export interface GroupNode {
  type: 'group';
  combinator: Combinator;
  children: QueryNode[];
}

export type QueryNode = CriterionNode | GroupNode;

// The chart palette has eight slots and never cycles; the builder keeps the
// criterion count inside them so every criterion holds a colour of its own
export const MAX_CRITERIA = 8;

// Nesting depth of the builder tree, root group included
export const MAX_DEPTH = 4;

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

/** All criterion rows of a builder tree in depth-first order. */
export function flatCriteria(node: QueryNode): CriterionNode[] {
  if (node.type === 'criterion') return [node];
  return node.children.flatMap(flatCriteria);
}

/** The boolean condition the tree expresses over the positive, non-empty
 * criteria: negations and empty rows are dropped, empty groups vanish and
 * single-child groups collapse into their only child. Null when nothing
 * positive remains. */
function prune(node: QueryNode): QueryNode | null {
  if (node.type === 'criterion') {
    return node.text !== '' && !isNegated(node.mode) && !node.disabled ? node : null;
  }
  const children = node.children.map(prune).filter((child): child is QueryNode => child !== null);
  if (children.length === 0) return null;
  if (children.length === 1) return children[0];
  return { type: 'group', combinator: node.combinator, children };
}

function hasAlternation(node: QueryNode): boolean {
  if (node.type === 'criterion') return false;
  return node.combinator === 'any' || node.children.some(hasAlternation);
}

/** The tree as one zero-width assertion: a criterion is `(?=.*X)`, an ALL
 * group concatenates its children, an ANY group alternates them. Bodies are
 * non-capturing here; colours come from the capture probes appended after. */
function assertion(node: QueryNode): string {
  if (node.type === 'criterion') return `(?=.*${body(node)})`;
  const parts = node.children.map(assertion);
  return node.combinator === 'all' ? parts.join('') : `(?:${parts.join('|')})`;
}

/**
 * Compiles the builder tree into a single plain regex. Negations always
 * exclude the whole line, wherever they sit in the tree.
 *
 * - Flat ANY without NOTs: `(?<a>A)|(?<b>B)` - plain alternation, fast engine.
 * - No ANY anywhere: `^(?=.*?(?<a>A))(?=.*?(?<b>B))` - one lookahead per
 *   criterion, captures inline since every lookahead is always evaluated.
 *   The prefix before a capture is lazy: a greedy `.*` backtracks from the
 *   line end and captures the last possible occurrence, truncating it when
 *   the criterion can also match a suffix of itself (`\d+ms` on "10.28ms"
 *   would capture just "8ms"). Lazy finds the earliest, full occurrence.
 * - Anything else, e.g. (A OR B) AND C: the tree compiles to nested
 *   zero-width assertions (`^(?:(?=.*A)|(?=.*B))(?=.*C)`), followed by gated
 *   capture probes (see colorProbes). Each probe always succeeds, so it never
 *   constrains the match, but a criterion is only captured - and thus coloured
 *   - when the ALL group it sits in is fully satisfied. That way a filter text
 *   shared by two groups does not pick up the colour of a group whose other
 *   criteria the line fails.
 */
export function compileQuery(root: GroupNode): string {
  const used = flatCriteria(root).filter(
    (criterion) => criterion.text !== '' && !criterion.disabled,
  );
  const positives = used.filter((criterion) => !isNegated(criterion.mode));
  const negatives = used.filter((criterion) => isNegated(criterion.mode));
  const names = positiveGroupNames(positives);
  const nameOf = new Map(positives.map((criterion, i) => [criterion, names[i]]));
  const capture = (criterion: CriterionNode) => `(?<${nameOf.get(criterion)}>${body(criterion)})`;
  const nots = negatives.map((criterion) => `(?!.*${body(criterion)})`);

  if (positives.length === 0) {
    return nots.length === 0 ? '' : `^${nots.join('')}`;
  }
  if (positives.length === 1 && nots.length === 0) {
    return capture(positives[0]);
  }
  const condition = prune(root) as QueryNode;
  if (!hasAlternation(condition)) {
    return `^${positives.map((criterion) => `(?=.*?${capture(criterion)})`).join('')}${nots.join('')}`;
  }
  const flatAny =
    condition.type === 'group' &&
    condition.combinator === 'any' &&
    condition.children.every((child) => child.type === 'criterion');
  if (flatAny && nots.length === 0) {
    return positives.map(capture).join('|');
  }
  // Gated colour probes over the pruned (positives-only) tree: a criterion is
  // captured only when every ALL group enclosing it matches as a whole, while
  // ANY children are probed independently so each matching branch keeps its
  // colour. Every fragment ends in `|`, so it always succeeds and leaves the
  // match to the leading assertion; captures land in tree order, one per slot.
  const colorProbes = (node: QueryNode): string => {
    if (node.type === 'criterion') return `(?=.*?${capture(node)}|)`;
    const inner = node.children.map(colorProbes).join('');
    // ANY: each child colours on its own; ALL: only when the group is satisfied
    return node.combinator === 'any' ? inner : `(?=${assertion(node)}${inner}|)`;
  };
  return `^${assertion(condition)}${nots.join('')}${colorProbes(condition)}`;
}

// --- Highlighting ---

export interface HighlightPart {
  text: string;
  // null: plain text; 0: match without a group; 1-based colour slot otherwise
  slot: number | null;
  // Every colour slot covering this exact run, set only when more than one
  // criterion captured the same characters (e.g. two rules sharing a filter).
  // Rendered as diagonal stripes; `slot` stays the first of these.
  slots?: number[];
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

// The translucent fill a single slot paints, reused by the striped background
function slotColor(slot: number): string {
  if (slot === 0) return 'color-mix(in srgb, #eab308 40%, transparent)';
  return `color-mix(in srgb, var(--chart-${((slot - 1) % 8) + 1}) 40%, transparent)`;
}

/** The CSS class for a highlight part: the single slot colour, or none when the
 * part carries several colours - those are striped by partStyle instead. */
export function partClass(part: HighlightPart): string {
  if (part.slots && part.slots.length > 1) return '';
  return slotClass(part.slot);
}

/** Diagonal-stripe background when several colours cover the same characters
 * (two rules sharing one filter text), else undefined so partClass applies. */
export function partStyle(part: HighlightPart): string | undefined {
  const slots = part.slots;
  if (!slots || slots.length <= 1) return undefined;
  const band = 7;
  const stops = slots
    .map((slot, i) => `${slotColor(slot)} ${i * band}px ${(i + 1) * band}px`)
    .join(', ');
  return `border-radius: 2px; background: repeating-linear-gradient(45deg, ${stops});`;
}

/** The part of a line the query isolates: every named capture group that took
 * part, space-joined in pattern order, or the whole matched text when the
 * pattern has no named groups. Unnamed `(...)` groups are plain grouping here,
 * exactly as in highlighting - only `(?<x>...)` isolates. Null when the line
 * does not match (or the query cannot compile as a JS regex). Mirrors the
 * backend's Matcher::extract so the table and the export agree. */
export function buildExtractor(
  query: string,
  isRegex: boolean,
  caseSensitive: boolean,
  disabled?: ReadonlySet<string>,
): ((line: string) => string | null) | null {
  if (!query) return null;
  const source = isRegex ? query : query.replace(ESCAPE, '\\$&');
  let regex: RegExp;
  try {
    regex = new RegExp(source, caseSensitive ? '' : 'i');
  } catch {
    return null;
  }
  // Groups the user toggled off drop out of the isolated value, exactly as the
  // backend's Matcher::extract skips them, so the preview and the export agree.
  const names = namedGroupsOf(source).filter((name) => !disabled?.has(name));
  return (line: string): string | null => {
    const match = regex.exec(line);
    if (!match) return null;
    if (names.length === 0) return match[0];
    const joined = names
      .map((name) => match.groups?.[name])
      .filter((value): value is string => value !== undefined && value !== '')
      .join(' ');
    return joined === '' ? match[0] : joined;
  };
}

/** Like buildExtractor, but keeps each capture group's colour instead of
 * flattening to a plain string: every participating named group becomes a
 * coloured part in its slot, single spaces between them are plain. Falls back
 * to the whole matched text (slot 0) when the pattern has no named groups, or
 * none of the enabled ones took part. Null when the line does not match. */
export function buildExtractorParts(
  query: string,
  isRegex: boolean,
  caseSensitive: boolean,
  disabled?: ReadonlySet<string>,
): ((line: string) => HighlightPart[] | null) | null {
  if (!query) return null;
  const source = isRegex ? query : query.replace(ESCAPE, '\\$&');
  let regex: RegExp;
  try {
    regex = new RegExp(source, caseSensitive ? '' : 'i');
  } catch {
    return null;
  }
  const slots = new Map(namedGroupsOf(source).map((name, i) => [name, i + 1]));
  const names = [...slots.keys()].filter((name) => !disabled?.has(name));
  const whole = (match: RegExpExecArray): HighlightPart[] => [{ text: match[0], slot: 0 }];
  return (line: string): HighlightPart[] | null => {
    const match = regex.exec(line);
    if (!match) return null;
    if (names.length === 0) return whole(match);
    const parts: HighlightPart[] = [];
    for (const name of names) {
      const value = match.groups?.[name];
      if (value === undefined || value === '') continue;
      if (parts.length > 0) parts.push({ text: ' ', slot: null });
      parts.push({ text: value, slot: slots.get(name) ?? 0 });
    }
    return parts.length === 0 ? whole(match) : parts;
  };
}

export interface Highlighter {
  parts(text: string): HighlightPart[];
}

const PLAIN = (text: string): HighlightPart[] => [{ text, slot: null }];

/** Splits lines into plain and matched segments, coloured by capture group.
 * Returns null when the query is empty or uses syntax JS regex lacks - the
 * match list still works then and only the inline highlighting is skipped. */
export function buildHighlighter(
  query: string,
  isRegex: boolean,
  caseSensitive: boolean,
  disabled?: ReadonlySet<string>,
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
      let namedSpans = 0;
      for (const [name, span] of Object.entries(match.indices?.groups ?? {})) {
        if (!span || span[1] <= span[0]) continue;
        // Count every participating named group, but only paint the enabled
        // ones: a group toggled off leaves its characters as plain text rather
        // than falling through to the whole-match highlight below.
        namedSpans++;
        if (disabled?.has(name)) continue;
        spans.push({ start: span[0], end: span[1], slot: slots.get(name) ?? 0 });
      }
      if (namedSpans === 0 && match[0].length > 0) {
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
    for (let i = 0; i < spans.length; i++) {
      const span = spans[i];
      if (span.start < at) continue;
      if (span.start > at) result.push({ text: text.slice(at, span.start), slot: null });
      // Fold every following span covering the exact same characters into one
      // striped part: two rules that share a filter text capture the same run,
      // so their colours must both show instead of one silently winning.
      const merged: number[] = [span.slot];
      while (i + 1 < spans.length && spans[i + 1].start === span.start && spans[i + 1].end === span.end) {
        if (!merged.includes(spans[i + 1].slot)) merged.push(spans[i + 1].slot);
        i++;
      }
      const part: HighlightPart = { text: text.slice(span.start, span.end), slot: span.slot };
      if (merged.length > 1) part.slots = merged;
      result.push(part);
      at = span.end;
    }
    if (at < text.length) result.push({ text: text.slice(at), slot: null });
    return result;
  }

  return { parts };
}
