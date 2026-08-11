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
  // Optional name for the whole set, shown on the builder row and on the
  // results legend chip; empty falls back to "Set 1", "Set 2", ...
  label?: string;
  // Muted like a criterion, but for the whole subtree: kept in the tree and
  // storage, left out of the compiled regex, colour slots and highlighting
  disabled?: boolean;
  // Folded in the builder so its rows are out of the way. Purely a view state,
  // persisted with the tree but ignored by everything that compiles it
  collapsed?: boolean;
}

export type QueryNode = CriterionNode | GroupNode;

/** A nested group as the results legend sees it: one colour and the capture
 * groups it owns, so the whole set toggles as a unit. */
export interface QuerySet {
  label: string;
  slot: number;
  names: string[];
}

// The chart palette. Criteria and groups take one slot each in tree order and
// wrap around it, so a tree of any size and depth still colours every row - the
// ninth criterion shares the first colour rather than capping the builder.
//
// The tree itself is uncapped. The one place size still shows is the backend's
// per-hit group bitmask (search.rs), which records the first 32 capture groups:
// past that a group still highlights and extracts (both client-side), but the
// legend cannot hide lines only it matched and it gets no "Matched group"
// chart series.
export const PALETTE_SLOTS = 8;

/** The palette slot a 1-based position wears, wrapping past the last colour. */
export function paletteSlot(position: number): number {
  return ((position - 1) % PALETTE_SLOTS) + 1;
}

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

/** The criterion rows that take part in the query, in depth-first order: a
 * muted row is skipped, and so is every row inside a muted group. */
export function enabledCriteria(node: QueryNode): CriterionNode[] {
  if (node.disabled) return [];
  if (node.type === 'criterion') return [node];
  return node.children.flatMap(enabledCriteria);
}

/** The nested groups of a tree in depth-first order, root excluded - the root
 * is the query itself, so it carries no colour of its own and cannot be muted.
 * Muted groups stay in the list so their colour does not shift when toggled. */
export function flatGroups(root: GroupNode): GroupNode[] {
  return root.children.flatMap((child) =>
    child.type === 'group' ? [child, ...flatGroups(child)] : [],
  );
}

/** Palette slot of every nested group, from the same eight colours the
 * criteria use. Groups wear theirs as a border and a legend chip rather than a
 * filled dot, so sharing a colour with a criterion still reads apart. */
export function groupSlots(root: GroupNode): Map<GroupNode, number> {
  return new Map(flatGroups(root).map((group, i) => [group, paletteSlot(i + 1)]));
}

/** The capture-group name of every criterion that compiles to one, in pattern
 * order. Shared by the compiler and the set legend so both agree on names. */
function captureNames(root: GroupNode): Map<CriterionNode, string> {
  const positives = enabledCriteria(root).filter(
    (criterion) => criterion.text !== '' && !isNegated(criterion.mode),
  );
  const names = positiveGroupNames(positives);
  return new Map(positives.map((criterion, i) => [criterion, names[i]]));
}

/** One legend entry per nested group: its colour and the capture groups of the
 * criteria inside it, subgroups included, so the results legend can switch a
 * whole set off at once. Groups that contribute no capture group - muted, all
 * negative, or empty - are left out. */
export function querySets(root: GroupNode): QuerySet[] {
  const nameOf = captureNames(root);
  const slots = groupSlots(root);
  const sets: QuerySet[] = [];
  flatGroups(root).forEach((group, i) => {
    const names = enabledCriteria(group)
      .map((criterion) => nameOf.get(criterion))
      .filter((name): name is string => name !== undefined);
    if (names.length === 0) return;
    sets.push({
      label: group.label?.trim() || `Set ${i + 1}`,
      slot: slots.get(group) ?? 1,
      names,
    });
  });
  return sets;
}

/** The boolean condition the tree expresses over the positive, non-empty
 * criteria: negations and empty rows are dropped, empty groups vanish and
 * single-child groups collapse into their only child. Null when nothing
 * positive remains. */
function prune(node: QueryNode): QueryNode | null {
  if (node.disabled) return null;
  if (node.type === 'criterion') {
    return node.text !== '' && !isNegated(node.mode) ? node : null;
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
 * exclude the whole entry, wherever they sit in the tree: they compile to
 * `(?!...)` right after the leading `^`, and the backend lifts that run of
 * leading negative look-aheads out of the pattern and tests it against every
 * line of an entry (a hit is an entry, so excluding only the line the term sits
 * on would still let the entry through on one of its stack-trace lines).
 * Keeping them at the front is what makes them liftable - do not move them.
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
  const used = enabledCriteria(root).filter((criterion) => criterion.text !== '');
  const positives = used.filter((criterion) => !isNegated(criterion.mode));
  const negatives = used.filter((criterion) => isNegated(criterion.mode));
  const nameOf = captureNames(root);
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
    return `^${nots.join('')}${positives.map((criterion) => `(?=.*?${capture(criterion)})`).join('')}`;
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
  return `^${nots.join('')}${assertion(condition)}${colorProbes(condition)}`;
}

// --- Parsing back ---
//
// The inverse of compileQuery, so a pattern the builder wrote - saved, reloaded
// or hand-edited - opens back up as the criteria it came from instead of an
// empty builder. Strict on purpose: anything that is not shaped like compiled
// output returns null and the builder keeps the tree it had, rather than
// mangling a hand-written regex into criteria that do not mean the same thing.
// What the pattern does not carry is lost either way: muted rows, and the names
// of groups that hold no criterion.

const UNESCAPE = /\\([.*+?^${}()|[\]\\])/g;

/** Index of the `)` closing the group that opens at `at`, honouring escapes and
 * character classes (where a paren is a literal). -1 when it never closes. */
function groupEnd(pattern: string, at: number): number {
  let depth = 0;
  let inClass = false;
  for (let i = at; i < pattern.length; i++) {
    const char = pattern[i];
    if (char === '\\') i++;
    else if (inClass) {
      if (char === ']') inClass = false;
    } else if (char === '[') inClass = true;
    else if (char === '(') depth++;
    else if (char === ')' && --depth === 0) return i;
  }
  return -1;
}

/** Splits on the `|` that alternate the whole pattern - those outside every
 * group and character class. */
function splitAlternatives(source: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let inClass = false;
  let start = 0;
  for (let i = 0; i < source.length; i++) {
    const char = source[i];
    if (char === '\\') i++;
    else if (inClass) {
      if (char === ']') inClass = false;
    } else if (char === '[') inClass = true;
    else if (char === '(') depth++;
    else if (char === ')') depth--;
    else if (char === '|' && depth === 0) {
      parts.push(source.slice(start, i));
      start = i + 1;
    }
  }
  parts.push(source.slice(start));
  return parts;
}

/** The top-level groups of `source`, in order. Null when anything sits between
 * or outside them, which compiled output never has. */
function topLevelGroups(source: string): string[] | null {
  const groups: string[] = [];
  let at = 0;
  while (at < source.length) {
    if (source[at] !== '(') return null;
    const end = groupEnd(source, at);
    if (end === -1) return null;
    groups.push(source.slice(at, end + 1));
    at = end + 1;
  }
  return groups;
}

/** The criterion row one compiled body came from: a body that is exactly an
 * escaped literal was a "contains" row, anything else a regex one. */
function criterionOf(body: string, negated: boolean): CriterionNode {
  const text = body.replace(UNESCAPE, '$1');
  const literal = text.replace(ESCAPE, '\\$&') === body;
  const mode: CriterionMode = negated
    ? literal
      ? 'notContains'
      : 'notRegex'
    : literal
      ? 'contains'
      : 'regex';
  return { type: 'criterion', mode, text: literal ? text : body, label: '' };
}

// g1, g2... are what an unlabelled criterion compiles to, so they read back as
// no label rather than as one the user never typed
function labelOf(name: string): string {
  return /^g\d+$/.test(name) ? '' : name;
}

/** A whole-string `(?<name>body)`, the shape a lone positive criterion (or one
 * branch of a flat OR) compiles to. */
function wholeCapture(source: string): CriterionNode | null {
  if (!source.startsWith('(?<') || groupEnd(source, 0) !== source.length - 1) return null;
  const close = source.indexOf('>');
  const name = source.slice(3, close);
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) return null;
  const criterion = criterionOf(source.slice(close + 1, -1), false);
  criterion.label = labelOf(name);
  return criterion;
}

/** The named capture groups of a colour probe, in the order the probe lists
 * them - which is the order the criteria sit in the tree. */
function probeNames(probe: string): string[] {
  return namedGroupsOf(probe);
}

/** One assertion group as a tree node: `(?=.*X)` and `(?=.*?(?<n>X))` are
 * criteria, `(?:a|b)` is an ANY group whose branches are assertion sequences.
 * Criteria that carry no name of their own take one from the probes later. */
function parseAssertion(group: string): QueryNode | null {
  if (group.startsWith('(?:')) {
    const branches = splitAlternatives(group.slice(3, -1));
    if (branches.length < 2) return null;
    const children: QueryNode[] = [];
    for (const branch of branches) {
      const nodes = parseAssertions(branch);
      if (!nodes) return null;
      children.push(
        nodes.length === 1 ? nodes[0] : { type: 'group', combinator: 'all', children: nodes },
      );
    }
    return { type: 'group', combinator: 'any', children };
  }
  if (!group.startsWith('(?=')) return null;
  const inner = group.slice(3, -1);
  // The ALL form captures inline, so its criteria arrive already named
  if (inner.startsWith('.*?(?<')) {
    const capture = wholeCapture(inner.slice(3));
    return capture;
  }
  if (!inner.startsWith('.*')) return null;
  return criterionOf(inner.slice(2), false);
}

function parseAssertions(source: string): QueryNode[] | null {
  const groups = topLevelGroups(source);
  if (!groups) return null;
  const nodes: QueryNode[] = [];
  for (const group of groups) {
    const node = parseAssertion(group);
    if (!node) return null;
    nodes.push(node);
  }
  return nodes;
}

/** Criteria of a tree in depth-first order - the order the compiler names them
 * in, so probe names pair up with the criteria that lack one. */
function parsedCriteria(node: QueryNode): CriterionNode[] {
  return node.type === 'criterion' ? [node] : node.children.flatMap(parsedCriteria);
}

/**
 * Reads a compiled pattern back into a builder tree, or null when the pattern
 * was not compiled by the builder. Exclusions always land as the last rows of
 * the root group, wherever the pattern put them, so reopening the builder does
 * not shuffle the rows around.
 */
export function parseQuery(pattern: string): GroupNode | null {
  if (!pattern) return null;

  const single = wholeCapture(pattern);
  if (single) return { type: 'group', combinator: 'all', children: [single] };

  // A flat OR compiles to bare alternation
  const alternatives = splitAlternatives(pattern);
  if (alternatives.length > 1) {
    const children = alternatives.map(wholeCapture);
    if (children.some((child) => child === null)) return null;
    return { type: 'group', combinator: 'any', children: children as CriterionNode[] };
  }

  if (!pattern.startsWith('^')) return null;
  const groups = topLevelGroups(pattern.slice(1));
  if (!groups) return null;

  const nots: CriterionNode[] = [];
  const structure: string[] = [];
  const names: string[] = [];
  for (const group of groups) {
    if (group.startsWith('(?!.*')) {
      nots.push(criterionOf(group.slice(5, -1), true));
    } else if (group.startsWith('(?=') && group.endsWith('|)')) {
      // A colour probe: it constrains nothing and only carries the names
      names.push(...probeNames(group));
    } else {
      structure.push(group);
    }
  }

  const parsed = structure.map(parseAssertion);
  if (parsed.some((node) => node === null)) return null;
  const nodes = parsed as QueryNode[];

  // Criteria the assertion could not name take theirs from the probes, in the
  // same depth-first order the compiler wrote them
  const criteria = nodes.flatMap(parsedCriteria);
  let next = 0;
  for (const criterion of criteria) {
    if (criterion.label === '' && next < names.length) criterion.label = labelOf(names[next++]);
  }

  if (criteria.length + nots.length === 0) return null;

  // A lone OR group is the query itself; anything else hangs under an ALL root
  return nodes.length === 1 && nodes[0].type === 'group' && nots.length === 0
    ? nodes[0]
    : { type: 'group', combinator: 'all', children: [...nodes, ...nots] };
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
  return `hl-${paletteSlot(slot)}`;
}

// The translucent fill a single slot paints, reused by the striped background
function slotColor(slot: number): string {
  if (slot === 0) return 'color-mix(in srgb, #eab308 40%, transparent)';
  return `color-mix(in srgb, var(--chart-${paletteSlot(slot)}) 40%, transparent)`;
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
  const fromOwnLine = (own: string): string | null => {
    const match = regex.exec(own);
    if (!match) return null;
    if (names.length === 0) return match[0];
    const joined = names
      .map((name) => match.groups?.[name])
      .filter((value): value is string => value !== undefined && value !== '')
      .join(' ');
    return joined === '' ? match[0] : joined;
  };
  // The line is a whole entry, matched line by line by the backend (see the
  // highlighter): the value comes from the first of its lines that matches, so
  // an entry whose match sits on a continuation line still isolates something.
  return (line: string): string | null =>
    line.includes('\n') ? firstOf(line, fromOwnLine) : fromOwnLine(line);
}

/** `pick` run over each physical line of an entry, first non-null wins. */
function firstOf<T>(entry: string, pick: (line: string) => T | null): T | null {
  for (const own of entry.split('\n')) {
    const value = pick(own);
    if (value !== null) return value;
  }
  return null;
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
  const fromOwnLine = (own: string): HighlightPart[] | null => {
    const match = regex.exec(own);
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
  // Per physical line, exactly as buildExtractor
  return (line: string): HighlightPart[] | null =>
    line.includes('\n') ? firstOf(line, fromOwnLine) : fromOwnLine(line);
}

export interface Highlighter {
  parts(text: string): HighlightPart[];
}

const PLAIN = (text: string): HighlightPart[] => [{ text, slot: null }];

// Matches walked per entry before highlighting gives up: a 64 KB SQL body would
// otherwise cost thousands of spans to paint runs nobody scrolls to.
const MATCH_ROUNDS = 200;

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

  /** One physical line, split into plain and coloured runs. `budget` caps the
   * matches walked, shared across an entry's lines by `parts`. */
  function lineOwnParts(text: string, budget: { left: number }): HighlightPart[] {
    if (!text) return PLAIN(text);
    const spans: { start: number; end: number; slot: number }[] = [];
    regex.lastIndex = 0;
    let match: RegExpExecArray | null;
    // Capped so a megabyte SQL body cannot stall rendering
    while (budget.left > 0 && (match = regex.exec(text))) {
      budget.left--;
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
      while (
        i + 1 < spans.length &&
        spans[i + 1].start === span.start &&
        spans[i + 1].end === span.end
      ) {
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

  /** A hit is a whole entry - its header line plus every continuation line,
   * newline-joined - and the backend tested each of those lines on its own.
   * The highlighter has to do the same: run over the join instead and `^` only
   * anchors at the entry's first line while `.` never reaches past a newline,
   * so a match sitting on a stack-trace or SQL-body line goes uncoloured
   * everywhere, in the row and in the expanded view alike. */
  function parts(text: string): HighlightPart[] {
    const budget = { left: MATCH_ROUNDS };
    if (!text.includes('\n')) return lineOwnParts(text, budget);
    const result: HighlightPart[] = [];
    const lines = text.split('\n');
    for (let i = 0; i < lines.length; i++) {
      if (i > 0) result.push({ text: '\n', slot: null });
      if (lines[i]) result.push(...lineOwnParts(lines[i], budget));
    }
    return result;
  }

  return { parts };
}
