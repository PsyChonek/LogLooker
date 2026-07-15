<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue';
import type { ChartSeries } from '@/types';

// Hand-rolled SVG so the chart follows the app's theme and needs no dependency.
// Colours come from the --chart-N slots in style.css, assigned to series in a
// fixed order and never cycled — the backend's top-N cut keeps it under eight.
const MAX_SLOTS = 8;
const ROW_HEIGHT = 26;
const BAR_THICKNESS = 14;
// A 2px surface gap separates fills instead of a border around them
const GAP = 2;
const CORNER = 4;

const props = withDefaults(
  defineProps<{
    labels: string[];
    series: ChartSeries[];
    type: 'bar' | 'line';
    stacked?: boolean;
    horizontal?: boolean;
    metricLabel: string;
    // Short form for the axis, long form for the tooltip
    formatX?: (label: string) => string;
    formatXLong?: (label: string) => string;
    height?: number;
  }>(),
  {
    stacked: false,
    horizontal: false,
    formatX: (label: string) => label,
    formatXLong: (label: string) => label,
    height: 360,
  },
);

const container = ref<HTMLElement | null>(null);
const width = ref(800);
let observer: ResizeObserver | null = null;

onMounted(() => {
  if (!container.value) return;
  width.value = container.value.clientWidth;
  observer = new ResizeObserver(() => {
    if (container.value) width.value = container.value.clientWidth;
  });
  observer.observe(container.value);
});

onUnmounted(() => observer?.disconnect());

const drawn = computed(() => props.series.slice(0, MAX_SLOTS));

function color(index: number): string {
  return `var(--chart-${(index % MAX_SLOTS) + 1})`;
}

// Line charts with a handful of series get their name at the last point, so
// identity never rests on colour alone; more than that would collide.
const directLabels = computed(
  () => props.type === 'line' && drawn.value.length > 1 && drawn.value.length <= 4,
);

const margin = computed(() => {
  if (props.horizontal) return { top: 8, right: 76, bottom: 34, left: 176 };
  return { top: 16, right: directLabels.value ? 104 : 20, bottom: 44, left: 64 };
});

const chartHeight = computed(() =>
  props.horizontal
    ? props.labels.length * ROW_HEIGHT + margin.value.top + margin.value.bottom
    : props.height,
);

const plot = computed(() => ({
  width: Math.max(10, width.value - margin.value.left - margin.value.right),
  height: Math.max(10, chartHeight.value - margin.value.top - margin.value.bottom),
}));

const maxValue = computed(() => {
  let max = 0;
  if (props.stacked && !props.horizontal) {
    props.labels.forEach((_, i) => {
      const sum = drawn.value.reduce((acc, s) => acc + (s.values[i] ?? 0), 0);
      max = Math.max(max, sum);
    });
  } else {
    for (const s of drawn.value) {
      for (const v of s.values) max = Math.max(max, v ?? 0);
    }
  }
  return max;
});

/** 1, 2, 2.5, 5, 10 x a power of ten — the steps people read without effort. */
function niceStep(raw: number): number {
  const power = Math.pow(10, Math.floor(Math.log10(raw)));
  for (const m of [1, 2, 2.5, 5, 10]) {
    if (raw <= m * power) return m * power;
  }
  return 10 * power;
}

const ticks = computed(() => {
  const max = maxValue.value;
  if (max <= 0) return [0, 1];
  const step = niceStep(max / 5);
  const top = Math.ceil(max / step) * step;
  const out: number[] = [];
  for (let v = 0; v <= top + step / 2; v += step) out.push(Number(v.toFixed(10)));
  return out;
});

const axisMax = computed(() => ticks.value[ticks.value.length - 1] || 1);

const band = computed(() =>
  props.horizontal ? ROW_HEIGHT : plot.value.width / Math.max(1, props.labels.length),
);

function scale(value: number): number {
  return (value / axisMax.value) * (props.horizontal ? plot.value.width : plot.value.height);
}

function bandCenter(index: number): number {
  return (props.horizontal ? margin.value.top : margin.value.left) + band.value * (index + 0.5);
}

/** Bar with rounded corners on the data end only — the baseline end stays square. */
function barPath(x: number, y: number, w: number, h: number, end: 'top' | 'right'): string {
  if (w <= 0 || h <= 0) return '';
  const r = Math.min(CORNER, end === 'top' ? Math.min(w / 2, h) : Math.min(h / 2, w));
  if (end === 'top') {
    return `M${x},${y + h} L${x},${y + r} Q${x},${y} ${x + r},${y} L${x + w - r},${y} Q${x + w},${y} ${x + w},${y + r} L${x + w},${y + h} Z`;
  }
  return `M${x},${y} L${x + w - r},${y} Q${x + w},${y} ${x + w},${y + r} L${x + w},${y + h - r} Q${x + w},${y + h} ${x + w - r},${y + h} L${x},${y + h} Z`;
}

interface Mark {
  path: string;
  color: string;
  seriesIndex: number;
  labelIndex: number;
}

const bars = computed<Mark[]>(() => {
  if (props.type !== 'bar') return [];
  const marks: Mark[] = [];
  const { left, top } = margin.value;

  if (props.horizontal) {
    props.labels.forEach((_, i) => {
      const value = drawn.value[0]?.values[i];
      if (value == null || value <= 0) return;
      const y = top + i * ROW_HEIGHT + (ROW_HEIGHT - BAR_THICKNESS) / 2;
      marks.push({
        path: barPath(left, y, scale(value), BAR_THICKNESS, 'right'),
        color: color(0),
        seriesIndex: 0,
        labelIndex: i,
      });
    });
    return marks;
  }

  const baseline = top + plot.value.height;
  const slot = Math.max(1, Math.min(band.value - GAP, 56));
  const count = drawn.value.length;

  props.labels.forEach((_, i) => {
    const center = bandCenter(i);
    if (props.stacked || count === 1) {
      let cumulative = 0;
      // Bottom-up, so the 2px gap sits between a segment and the one below it
      // and the topmost segment keeps its true value at the rounded end.
      const segments = drawn.value
        .map((s, si) => ({ value: s.values[i] ?? 0, seriesIndex: si }))
        .filter((s) => s.value > 0);
      segments.forEach((segment, index) => {
        const yTop = baseline - scale(cumulative + segment.value);
        const yBottom = baseline - scale(cumulative);
        const raw = yBottom - yTop;
        const height = raw > GAP + 1 && index > 0 ? raw - GAP : raw;
        marks.push({
          path: barPath(center - slot / 2, yTop, slot, height, 'top'),
          color: color(segment.seriesIndex),
          seriesIndex: segment.seriesIndex,
          labelIndex: i,
        });
        cumulative += segment.value;
      });
    } else {
      const each = Math.max(1, (slot - (count - 1) * GAP) / count);
      drawn.value.forEach((s, si) => {
        const value = s.values[i];
        if (value == null || value <= 0) return;
        const x = center - slot / 2 + si * (each + GAP);
        const height = scale(value);
        marks.push({
          path: barPath(x, baseline - height, each, height, 'top'),
          color: color(si),
          seriesIndex: si,
          labelIndex: i,
        });
      });
    }
  });
  return marks;
});

interface LinePath {
  d: string;
  color: string;
  name: string;
  lastPoint: { x: number; y: number } | null;
}

const lines = computed<LinePath[]>(() => {
  if (props.type !== 'line') return [];
  const baseline = margin.value.top + plot.value.height;
  return drawn.value.map((s, si) => {
    let d = '';
    let pen = false;
    let lastPoint: { x: number; y: number } | null = null;
    props.labels.forEach((_, i) => {
      const value = s.values[i];
      if (value == null) {
        // A gap, not a zero — the line breaks rather than dipping to the floor
        pen = false;
        return;
      }
      const x = bandCenter(i);
      const y = baseline - scale(value);
      d += `${pen ? 'L' : 'M'}${x.toFixed(1)},${y.toFixed(1)} `;
      pen = true;
      lastPoint = { x, y };
    });
    return { d: d.trim(), color: color(si), name: s.name, lastPoint };
  });
});

// --- Hover ---

const hover = ref<{ index: number; x: number; y: number } | null>(null);

function onMove(event: MouseEvent) {
  if (props.labels.length === 0) return;
  const rect = (event.currentTarget as SVGElement).getBoundingClientRect();
  const offset = props.horizontal
    ? event.clientY - rect.top - margin.value.top
    : event.clientX - rect.left - margin.value.left;
  const index = Math.floor(offset / band.value);
  if (index < 0 || index >= props.labels.length) {
    hover.value = null;
    return;
  }
  hover.value = { index, x: event.clientX - rect.left, y: event.clientY - rect.top };
}

const hoverRows = computed(() => {
  if (!hover.value) return [];
  const i = hover.value.index;
  return drawn.value
    .map((s, si) => ({ name: s.name, value: s.values[i], color: color(si) }))
    .filter((row) => row.value != null);
});

const hoverPoints = computed(() => {
  if (!hover.value || props.type !== 'line') return [];
  const baseline = margin.value.top + plot.value.height;
  const i = hover.value.index;
  return drawn.value
    .map((s, si) => ({ value: s.values[i], color: color(si) }))
    .filter((p) => p.value != null)
    .map((p) => ({ cx: bandCenter(i), cy: baseline - scale(p.value as number), color: p.color }));
});

// Tooltip is clamped inside the container so it never runs off the edge
const tooltipStyle = computed(() => {
  if (!hover.value) return {};
  const left = Math.min(Math.max(hover.value.x + 14, 8), Math.max(8, width.value - 260));
  const top = Math.min(hover.value.y + 14, Math.max(8, chartHeight.value - 40));
  return { left: `${left}px`, top: `${top}px` };
});

function formatValue(value: number): string {
  const abs = Math.abs(value);
  if (abs >= 1_000_000_000) return `${(value / 1_000_000_000).toFixed(1)}B`;
  if (abs >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (abs >= 10_000) return `${(value / 1000).toFixed(0)}k`;
  // Round, never truncate: a tick sitting at 12.5 must not print as "13".
  // Trailing zeros go too — an axis reads 7.5, not 7.50.
  return Number(value.toFixed(2)).toLocaleString();
}

function formatExact(value: number): string {
  return Number.isInteger(value)
    ? value.toLocaleString()
    : value.toLocaleString(undefined, { maximumFractionDigits: 2 });
}

// Only every k-th x label is drawn; k is whatever keeps them from colliding
const xLabelStep = computed(() => {
  if (props.horizontal) return 1;
  const perLabel = 62;
  return Math.max(1, Math.ceil((props.labels.length * perLabel) / Math.max(1, plot.value.width)));
});

function truncate(text: string, max: number): string {
  return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}
</script>

<template>
  <div
    ref="container"
    class="relative w-full"
  >
    <svg
      :width="width"
      :height="chartHeight"
      class="block select-none"
      @mousemove="onMove"
      @mouseleave="hover = null"
    >
      <!-- Value axis: solid hairline gridlines, one shade off the surface -->
      <g v-if="!horizontal">
        <g
          v-for="tick in ticks"
          :key="tick"
        >
          <line
            :x1="margin.left"
            :x2="margin.left + plot.width"
            :y1="margin.top + plot.height - scale(tick)"
            :y2="margin.top + plot.height - scale(tick)"
            :stroke="tick === 0 ? 'var(--chart-axis)' : 'var(--chart-grid)'"
            stroke-width="1"
          />
          <text
            :x="margin.left - 10"
            :y="margin.top + plot.height - scale(tick) + 4"
            text-anchor="end"
            class="text-[10px] tabular-nums"
            fill="var(--chart-ink)"
          >
            {{ formatValue(tick) }}
          </text>
        </g>
      </g>
      <g v-else>
        <g
          v-for="tick in ticks"
          :key="tick"
        >
          <line
            :x1="margin.left + scale(tick)"
            :x2="margin.left + scale(tick)"
            :y1="margin.top"
            :y2="margin.top + plot.height"
            :stroke="tick === 0 ? 'var(--chart-axis)' : 'var(--chart-grid)'"
            stroke-width="1"
          />
          <text
            :x="margin.left + scale(tick)"
            :y="margin.top + plot.height + 20"
            text-anchor="middle"
            class="text-[10px] tabular-nums"
            fill="var(--chart-ink)"
          >
            {{ formatValue(tick) }}
          </text>
        </g>
      </g>

      <!-- Hovered band -->
      <rect
        v-if="hover"
        :x="horizontal ? margin.left : bandCenter(hover.index) - band / 2"
        :y="horizontal ? margin.top + hover.index * ROW_HEIGHT : margin.top"
        :width="horizontal ? plot.width : band"
        :height="horizontal ? ROW_HEIGHT : plot.height"
        fill="var(--chart-grid)"
        opacity="0.4"
      />

      <!-- Category names (horizontal) -->
      <template v-if="horizontal">
        <text
          v-for="(label, i) in labels"
          :key="`cat-${i}`"
          :x="margin.left - 10"
          :y="margin.top + i * ROW_HEIGHT + ROW_HEIGHT / 2 + 4"
          text-anchor="end"
          class="text-[11px]"
          fill="var(--chart-ink)"
        >
          <title>{{ label }}</title>
          {{ truncate(formatX(label), 26) }}
        </text>
      </template>

      <!-- Time / bucket labels (vertical) -->
      <template v-else>
        <text
          v-for="(label, i) in labels"
          v-show="i % xLabelStep === 0"
          :key="`x-${i}`"
          :x="bandCenter(i)"
          :y="margin.top + plot.height + 18"
          text-anchor="middle"
          class="text-[10px] tabular-nums"
          fill="var(--chart-ink)"
        >
          {{ formatX(label) }}
        </text>
      </template>

      <path
        v-for="(mark, i) in bars"
        :key="`bar-${i}`"
        :d="mark.path"
        :fill="mark.color"
      />

      <!-- Value at the end of each category bar, so no value needs a tooltip -->
      <template v-if="horizontal">
        <text
          v-for="(mark, i) in bars"
          :key="`val-${i}`"
          :x="margin.left + scale(series[0]?.values[mark.labelIndex] ?? 0) + 8"
          :y="margin.top + mark.labelIndex * ROW_HEIGHT + ROW_HEIGHT / 2 + 4"
          class="text-[10px] tabular-nums"
          fill="var(--chart-ink)"
        >
          {{ formatValue(series[0]?.values[mark.labelIndex] ?? 0) }}
        </text>
      </template>

      <path
        v-for="(line, i) in lines"
        :key="`line-${i}`"
        :d="line.d"
        fill="none"
        :stroke="line.color"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
      />

      <!-- Direct labels: identity without reading the legend -->
      <template v-if="directLabels">
        <text
          v-for="(line, i) in lines"
          v-show="line.lastPoint"
          :key="`dl-${i}`"
          :x="(line.lastPoint?.x ?? 0) + 8"
          :y="(line.lastPoint?.y ?? 0) + 3"
          class="text-[10px] font-medium"
          :fill="line.color"
        >
          {{ truncate(line.name, 14) }}
        </text>
      </template>

      <!-- Crosshair markers: 2px surface ring keeps them legible where lines overlap -->
      <circle
        v-for="(point, i) in hoverPoints"
        :key="`hp-${i}`"
        :cx="point.cx"
        :cy="point.cy"
        r="4"
        :fill="point.color"
        stroke="var(--chart-surface)"
        stroke-width="2"
      />
    </svg>

    <div
      v-if="hover && hoverRows.length"
      class="pointer-events-none absolute z-10 min-w-[10rem] max-w-[16rem] rounded-md border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800 shadow-lg px-2.5 py-1.5 text-[11px]"
      :style="tooltipStyle"
    >
      <div class="font-medium text-gray-700 dark:text-gray-200 tabular-nums">
        {{ formatXLong(labels[hover.index]) }}
      </div>
      <div class="mt-1 text-[10px] uppercase tracking-wide text-gray-400">
        {{ metricLabel }}
      </div>
      <div
        v-for="row in hoverRows"
        :key="row.name"
        class="flex items-center gap-2 mt-0.5"
      >
        <span
          class="w-2 h-2 rounded-sm shrink-0"
          :style="{ backgroundColor: row.color }"
        />
        <span class="truncate text-gray-600 dark:text-gray-300">{{ row.name }}</span>
        <span class="ml-auto tabular-nums text-gray-800 dark:text-gray-100">
          {{ formatExact(row.value as number) }}
        </span>
      </div>
    </div>
  </div>
</template>
