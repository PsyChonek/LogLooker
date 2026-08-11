<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue';

// A read-only cheat sheet for the regex used by search: the tokens the query
// field accepts, plus ready-made examples aimed at log lines. The search box
// filters both by syntax, description and example text, so a user can type
// "digit" or "\d" and land on the same row. Content only - nothing here runs a
// query, so it is safe to open at any time.

const emit = defineEmits<{ close: [] }>();

interface Entry {
  syntax: string;
  desc: string;
  example?: string;
}

interface Section {
  title: string;
  entries: Entry[];
}

const SECTIONS: Section[] = [
  {
    title: 'Characters',
    entries: [
      { syntax: '.', desc: 'Any character except a newline', example: 'a.c matches "abc", "a c"' },
      { syntax: '\\d  \\D', desc: 'A digit / anything but a digit', example: '\\d\\d\\d matches "404"' },
      { syntax: '\\w  \\W', desc: 'Word char (letter, digit, _) / non-word char', example: '\\w+ matches "user_01"' },
      { syntax: '\\s  \\S', desc: 'Whitespace / non-whitespace' },
      { syntax: '\\t  \\n', desc: 'A tab / a newline' },
      { syntax: '[abc]', desc: 'Any one of a, b or c', example: '[eE]rror matches "error" or "Error"' },
      { syntax: '[^abc]', desc: 'Any character except a, b or c' },
      { syntax: '[a-z]', desc: 'Any character in the range a to z', example: '[0-9a-f] matches one hex digit' },
    ],
  },
  {
    title: 'Anchors and boundaries',
    entries: [
      { syntax: '^', desc: 'Start of the line', example: '^ERROR matches lines starting with ERROR' },
      { syntax: '$', desc: 'End of the line', example: 'failed$ matches lines ending in "failed"' },
      { syntax: '\\b', desc: 'A word boundary', example: '\\bid\\b matches the whole word "id" only' },
      { syntax: '\\B', desc: 'Not a word boundary' },
    ],
  },
  {
    title: 'Quantifiers',
    entries: [
      { syntax: '*', desc: 'Zero or more of the item before it', example: 'ab*c matches "ac", "abc", "abbc"' },
      { syntax: '+', desc: 'One or more', example: '\\d+ matches "1" and "1234"' },
      { syntax: '?', desc: 'Optional - zero or one', example: 'colou?r matches "color" and "colour"' },
      { syntax: '{n}', desc: 'Exactly n times', example: '\\d{4} matches a 4-digit year' },
      { syntax: '{n,}', desc: 'At least n times' },
      { syntax: '{n,m}', desc: 'Between n and m times', example: '\\d{1,3} matches 1 to 3 digits' },
      { syntax: '*?  +?  ??', desc: 'Lazy - match as few characters as possible', example: '".*?" stops at the first closing quote' },
    ],
  },
  {
    title: 'Groups and alternation',
    entries: [
      { syntax: 'a|b', desc: 'Either a or b', example: 'warn|error matches either word' },
      { syntax: '( ... )', desc: 'A capturing group', example: '(error|warn)-\\d+ groups the level' },
      { syntax: '(?: ... )', desc: 'A group that does not capture', example: '(?:ab)+ repeats "ab" without a capture' },
      { syntax: '(?<name> ... )', desc: 'A named capture group - names become chart series and highlight colours', example: '(?<code>\\d{3})' },
    ],
  },
  {
    title: 'Escaping',
    entries: [
      { syntax: '\\.', desc: 'A literal dot (escape any special character)', example: '10\\.0\\.0\\.1 matches the literal IP' },
      { syntax: '\\\\', desc: 'A literal backslash' },
      { syntax: '\\( \\) \\[ \\]', desc: 'Literal brackets and parentheses' },
    ],
  },
  {
    title: 'Lookaround (backend only)',
    entries: [
      { syntax: '(?= ... )', desc: 'Followed by (the query builder uses these to combine criteria with AND)', example: '^(?=.*error)(?=.*timeout)' },
      { syntax: '(?! ... )', desc: 'Not followed by', example: 'error(?!.*retry) - errors with no retry after them' },
    ],
  },
  {
    title: 'Examples for logs',
    entries: [
      { syntax: '(?i)error|fail|exception', desc: 'Any common error keyword, case-insensitive' },
      { syntax: 'HTTP/\\d\\.\\d" (?<status>[45]\\d\\d)', desc: 'Capture 4xx and 5xx HTTP status codes' },
      { syntax: '\\b\\d{1,3}(\\.\\d{1,3}){3}\\b', desc: 'An IPv4 address' },
      { syntax: '[0-9a-fA-F]{8}-([0-9a-fA-F]{4}-){3}[0-9a-fA-F]{12}', desc: 'A GUID / UUID' },
      { syntax: '\\d{4}-\\d{2}-\\d{2}[ T]\\d{2}:\\d{2}:\\d{2}', desc: 'An ISO timestamp' },
      { syntax: 'took (?<ms>\\d+) ?ms', desc: 'Capture a duration in milliseconds' },
      { syntax: '(?i)\\b(null|undefined)\\b', desc: 'Lines mentioning null or undefined' },
    ],
  },
];

const search = ref('');

const filtered = computed<Section[]>(() => {
  const q = search.value.trim().toLowerCase();
  if (!q) return SECTIONS;
  return SECTIONS.map((section) => {
    const titleHit = section.title.toLowerCase().includes(q);
    const entries = titleHit
      ? section.entries
      : section.entries.filter(
          (e) =>
            e.syntax.toLowerCase().includes(q) ||
            e.desc.toLowerCase().includes(q) ||
            (e.example ?? '').toLowerCase().includes(q),
        );
    return { title: section.title, entries };
  }).filter((section) => section.entries.length > 0);
});

const empty = computed(() => filtered.value.length === 0);

function onKey(e: KeyboardEvent) {
  if (e.key === 'Escape') emit('close');
}

onMounted(() => window.addEventListener('keydown', onKey));
onUnmounted(() => window.removeEventListener('keydown', onKey));
</script>

<template>
  <Teleport to="body">
    <div
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/30 backdrop-blur-sm p-4"
      @click.self="emit('close')"
    >
      <div
        class="flex flex-col w-full max-w-2xl max-h-[85vh] bg-white dark:bg-gray-800 rounded-lg shadow-xl border border-gray-200 dark:border-gray-700"
      >
        <div
          class="flex items-center gap-3 px-5 py-3 border-b border-gray-200 dark:border-gray-700 shrink-0"
        >
          <h2 class="text-sm font-semibold text-gray-800 dark:text-gray-100">
            Regex guide
          </h2>
          <input
            v-model="search"
            type="text"
            spellcheck="false"
            autofocus
            placeholder="Filter syntax and examples..."
            class="flex-1 px-3 py-1.5 text-xs font-mono rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900"
          >
          <button
            class="shrink-0 px-2 py-1 text-xs border border-gray-300 dark:border-gray-600 rounded text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-200"
            title="Close (Esc)"
            @click="emit('close')"
          >
            Close
          </button>
        </div>

        <div class="overflow-y-auto px-5 py-4 text-xs">
          <p class="mb-4 text-gray-500 dark:text-gray-400">
            Turn on the "Regex" toggle to use these patterns; without it the query is matched as
            plain text. Matching is case-insensitive unless "Case sensitive" is on, or a pattern sets
            <code class="px-1 rounded bg-gray-100 dark:bg-gray-700 font-mono">(?i)</code> itself.
          </p>

          <div
            v-if="empty"
            class="py-8 text-center text-gray-400"
          >
            Nothing matches "{{ search }}".
          </div>

          <section
            v-for="section in filtered"
            :key="section.title"
            class="mb-5 last:mb-0"
          >
            <h3
              class="mb-2 text-[11px] font-semibold uppercase tracking-wide text-gray-400 dark:text-gray-500"
            >
              {{ section.title }}
            </h3>
            <ul class="flex flex-col gap-1.5">
              <li
                v-for="(entry, i) in section.entries"
                :key="i"
                class="flex flex-col gap-1 sm:flex-row sm:items-baseline sm:gap-3"
              >
                <code
                  class="shrink-0 sm:w-52 px-1.5 py-0.5 rounded bg-gray-100 dark:bg-gray-900 font-mono text-blue-600 dark:text-blue-400 whitespace-pre-wrap break-all"
                >{{ entry.syntax }}</code>
                <div class="text-gray-700 dark:text-gray-300">
                  {{ entry.desc }}
                  <span
                    v-if="entry.example"
                    class="block text-gray-400 dark:text-gray-500"
                  >{{ entry.example }}</span>
                </div>
              </li>
            </ul>
          </section>
        </div>
      </div>
    </div>
  </Teleport>
</template>
