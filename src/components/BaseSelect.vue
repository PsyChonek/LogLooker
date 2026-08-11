<script lang="ts">
export interface SelectOption<T extends string | number = string | number> {
  value: T;
  label: string;
  disabled?: boolean;
  /* Draws a separator above the option, to set an action apart from the choices. */
  separated?: boolean;
}
</script>

<script setup lang="ts" generic="T extends string | number">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue';

/*
 * Dropdown
 *
 * A native <select> hands its popup to the engine, which paints it from the
 * control's own computed colours - a translucent background composites over
 * white and the list comes out unreadable, and nothing about the popup follows
 * the theme. This renders the list itself, teleported to <body> so a table or
 * modal cannot clip it, and positioned against the trigger's viewport rect.
 */

defineOptions({ inheritAttrs: false });

const model = defineModel<T>({ required: true });

const props = withDefaults(
  defineProps<{
    options: readonly SelectOption<T>[];
    disabled?: boolean;
    /* Shown when the model matches no option. */
    placeholder?: string;
    size?: 'sm' | 'md';
    tone?: 'default' | 'positive';
    align?: 'left' | 'right';
  }>(),
  {
    disabled: false,
    placeholder: 'Select...',
    size: 'md',
    tone: 'default',
    align: 'left',
  },
);

const emit = defineEmits<{ change: [value: T] }>();

const SIZES = {
  sm: { trigger: 'gap-1 px-1.5 py-0.5 text-[10px]', option: 'px-2 py-1 text-[10px]', icon: 'size-2.5' },
  md: { trigger: 'gap-1.5 px-2 py-1 text-xs', option: 'px-2.5 py-1.5 text-xs', icon: 'size-3' },
} as const;

const TONES = {
  default: {
    rest: 'border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-700 dark:text-gray-200 hover:border-blue-400 dark:hover:border-blue-500',
    open: 'border-blue-500 bg-white dark:bg-gray-800 text-gray-700 dark:text-gray-200 ring-2 ring-blue-500/20',
  },
  positive: {
    rest: 'border-green-300 dark:border-green-500/40 bg-green-50 dark:bg-green-500/10 text-green-700 dark:text-green-400 font-semibold hover:border-green-500',
    open: 'border-green-500 bg-green-50 dark:bg-green-500/10 text-green-700 dark:text-green-400 font-semibold ring-2 ring-green-500/20',
  },
} as const;

const open = ref(false);
const active = ref(-1);
const trigger = ref<HTMLButtonElement | null>(null);
const panel = ref<HTMLElement | null>(null);
const panelStyle = ref<Record<string, string>>({});

const selected = computed(() => props.options.find((o) => o.value === model.value) ?? null);
const sizing = computed(() => SIZES[props.size]);

/* Gap between the trigger and the panel, and the panel's clearance from the edge. */
const GAP = 4;
const MAX_PANEL = 288;

function place() {
  const el = trigger.value;
  if (!el) return;
  const rect = el.getBoundingClientRect();
  const below = window.innerHeight - rect.bottom - GAP * 2;
  const above = rect.top - GAP * 2;
  // Flip up only when below is genuinely cramped and above is roomier, so the
  // list stays in its expected place for all the ordinary cases.
  const flip = below < 160 && above > below;
  const style: Record<string, string> = {
    minWidth: `${rect.width}px`,
    maxWidth: `calc(100vw - ${GAP * 4}px)`,
    maxHeight: `${Math.max(96, Math.min(MAX_PANEL, flip ? above : below))}px`,
  };
  if (flip) style.bottom = `${window.innerHeight - rect.top + GAP}px`;
  else style.top = `${rect.bottom + GAP}px`;
  if (props.align === 'right') style.right = `${Math.max(GAP, window.innerWidth - rect.right)}px`;
  else style.left = `${Math.max(GAP, rect.left)}px`;
  panelStyle.value = style;
}

function scrollToActive() {
  nextTick(() => {
    panel.value?.querySelector('[data-active="true"]')?.scrollIntoView({ block: 'nearest' });
  });
}

function openPanel() {
  if (props.disabled) return;
  active.value = props.options.findIndex((o) => o.value === model.value);
  open.value = true;
  place();
  scrollToActive();
}

function close() {
  open.value = false;
  active.value = -1;
}

function pick(option: SelectOption<T>) {
  if (option.disabled) return;
  model.value = option.value;
  emit('change', option.value);
  close();
  trigger.value?.focus();
}

/* Steps to the next selectable option, wrapping, and never lands on a disabled one. */
function move(delta: number) {
  const count = props.options.length;
  if (!count) return;
  let i = active.value;
  for (let step = 0; step < count; step++) {
    i = (i + delta + count) % count;
    if (!props.options[i].disabled) break;
  }
  active.value = i;
  scrollToActive();
}

function onKeydown(event: KeyboardEvent) {
  if (props.disabled) return;
  if (!open.value) {
    if (['Enter', ' ', 'ArrowDown', 'ArrowUp'].includes(event.key)) {
      event.preventDefault();
      openPanel();
    }
    return;
  }
  switch (event.key) {
    case 'Escape':
      event.preventDefault();
      close();
      break;
    case 'ArrowDown':
      event.preventDefault();
      move(1);
      break;
    case 'ArrowUp':
      event.preventDefault();
      move(-1);
      break;
    case 'Home':
      event.preventDefault();
      active.value = -1;
      move(1);
      break;
    case 'End':
      event.preventDefault();
      active.value = 0;
      move(-1);
      break;
    case 'Enter':
    case ' ': {
      event.preventDefault();
      const option = props.options[active.value];
      if (option) pick(option);
      break;
    }
    case 'Tab':
      close();
      break;
  }
}

function onPointerDown(event: MouseEvent) {
  if (!open.value) return;
  const target = event.target as Node;
  if (trigger.value?.contains(target) || panel.value?.contains(target)) return;
  close();
}

/* A native popup does not travel with the page either; closing keeps the
   panel from floating away from a trigger that scrolled out from under it. */
function onScroll(event: Event) {
  if (open.value && !panel.value?.contains(event.target as Node)) close();
}

onMounted(() => {
  document.addEventListener('mousedown', onPointerDown);
  document.addEventListener('scroll', onScroll, true);
  window.addEventListener('resize', close);
});

onBeforeUnmount(() => {
  document.removeEventListener('mousedown', onPointerDown);
  document.removeEventListener('scroll', onScroll, true);
  window.removeEventListener('resize', close);
});
</script>

<template>
  <button
    ref="trigger"
    v-bind="$attrs"
    type="button"
    class="inline-flex items-center justify-between rounded-md border transition-colors"
    :class="[
      sizing.trigger,
      open ? TONES[tone].open : TONES[tone].rest,
      disabled ? 'opacity-40 cursor-not-allowed' : '',
    ]"
    :disabled="disabled"
    :aria-expanded="open"
    aria-haspopup="listbox"
    @click="open ? close() : openPanel()"
    @keydown="onKeydown"
  >
    <span
      class="truncate"
      :class="selected ? '' : 'text-gray-400 dark:text-gray-500'"
    >
      {{ selected?.label ?? placeholder }}
    </span>
    <svg
      class="shrink-0 text-gray-400 transition-transform duration-150"
      :class="[sizing.icon, open ? 'rotate-180' : '']"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
    >
      <path d="M3 6l5 5 5-5" />
    </svg>
  </button>

  <Teleport to="body">
    <div
      v-if="open"
      ref="panel"
      role="listbox"
      class="fixed z-[60] overflow-y-auto p-1 rounded-lg border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800 shadow-xl shadow-black/10 dark:shadow-black/40"
      :style="panelStyle"
    >
      <template
        v-for="(option, i) in options"
        :key="option.value"
      >
        <div
          v-if="option.separated && i > 0"
          class="my-1 border-t border-gray-100 dark:border-gray-700"
        />
        <button
          type="button"
          role="option"
          :aria-selected="option.value === model"
          :data-active="i === active"
          class="w-full flex items-center gap-2 rounded-md text-left transition-colors"
          :class="[
            sizing.option,
            option.disabled ? 'opacity-40 cursor-not-allowed' : '',
            option.value === model
              ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-700 dark:text-blue-300 font-medium'
              : i === active
                ? 'bg-gray-100 dark:bg-gray-700 text-gray-800 dark:text-gray-100'
                : 'text-gray-700 dark:text-gray-300',
          ]"
          @mouseenter="option.disabled ? null : (active = i)"
          @click="pick(option)"
        >
          <span class="truncate">{{ option.label }}</span>
          <svg
            v-if="option.value === model"
            class="shrink-0 ml-auto"
            :class="sizing.icon"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <path d="M3 8.5l3.5 3.5L13 5" />
          </svg>
        </button>
      </template>
    </div>
  </Teleport>
</template>
