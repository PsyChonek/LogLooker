<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import Spinner from '@/components/Spinner.vue';
import { useAppStore } from '@/stores/appStore';
import type { PackInfo } from '@/types';

const store = useAppStore();
const error = ref<string | null>(null);
const busy = ref(false);
const expanded = ref<Set<string>>(new Set());

onMounted(() => {
  if (store.plugins.packs.length === 0) reload();
});

async function reload() {
  busy.value = true;
  error.value = null;
  try {
    await store.reloadPlugins();
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

async function toggle(packId: string, enabled: boolean) {
  busy.value = true;
  error.value = null;
  try {
    await store.setPackEnabled(packId, enabled);
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

async function openFolder() {
  error.value = null;
  try {
    await store.openPluginsDir();
  } catch (e) {
    error.value = String(e);
  }
}

function toggleDetails(packId: string) {
  const next = new Set(expanded.value);
  if (next.has(packId)) next.delete(packId);
  else next.add(packId);
  expanded.value = next;
}

/** Where a plugin came from, as one short phrase. */
function originText(pack: PackInfo): string {
  if (pack.origin === 'bundled') return 'bundled';
  return pack.overridesBundled ? 'user file (replaces bundled)' : 'user file';
}

function originPath(pack: PackInfo): string | null {
  return typeof pack.origin === 'object' ? pack.origin.user.path : null;
}

const SOURCE_LABELS: Record<string, string> = {
  kudu: 'Azure App Service (Kudu)',
  'local-folder': 'Local folder',
};

const DISCOVERY_LABELS: Record<string, string> = {
  none: 'services added by hand',
  static: 'ships a service list',
  scrape: 'discovers from a page',
};

/** How many services in the current config each plugin owns. */
const serviceCounts = computed(() => {
  const counts: Record<string, number> = {};
  for (const service of store.config.services) {
    counts[service.packId] = (counts[service.packId] ?? 0) + 1;
  }
  return counts;
});

// A disabled plugin is not loaded, so it is known only by its id
const disabledBundled = computed(() =>
  store.plugins.disabled.filter((id) => store.plugins.bundled.includes(id)),
);
</script>

<template>
  <div>
    <div
      v-if="error"
      class="mb-4 px-4 py-2 rounded-md bg-red-50 dark:bg-red-900/30 border border-red-200 dark:border-red-800 text-sm text-red-700 dark:text-red-400 whitespace-pre-line"
    >
      {{ error }}
    </div>

    <div class="flex items-center gap-3 mb-4">
      <button
        class="px-3 py-1.5 text-xs font-medium border border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 rounded-md transition-colors"
        :disabled="busy"
        @click="reload"
      >
        {{ busy ? 'Reloading...' : 'Reload plugins' }}
      </button>
      <button
        class="px-3 py-1.5 text-xs font-medium border border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 rounded-md transition-colors"
        @click="openFolder"
      >
        Open plugins folder
      </button>
      <span
        class="text-xs text-gray-400 dark:text-gray-500 font-mono truncate"
        :title="store.plugins.pluginsDir"
      >
        {{ store.plugins.pluginsDir }}
      </span>
      <Spinner v-if="busy" />
    </div>

    <!-- A plugin that would not load has to be visible as broken, not absent -->
    <div
      v-if="store.plugins.errors.length"
      class="mb-4 rounded-lg border border-amber-200 dark:border-amber-800 bg-amber-50 dark:bg-amber-900/20 overflow-hidden"
    >
      <div
        class="px-4 py-2 text-xs font-semibold text-amber-800 dark:text-amber-300 border-b border-amber-200 dark:border-amber-800"
      >
        {{ store.plugins.errors.length }} plugin(s) failed to load
      </div>
      <div
        v-for="failure in store.plugins.errors"
        :key="failure.source"
        class="px-4 py-2 text-xs border-b border-amber-100 dark:border-amber-900/50 last:border-0"
      >
        <span class="font-mono font-semibold text-amber-800 dark:text-amber-300">{{
          failure.source
        }}</span>
        <div class="mt-0.5 text-amber-700 dark:text-amber-400 whitespace-pre-line">
          {{ failure.message }}
        </div>
      </div>
    </div>

    <div class="space-y-3">
      <div
        v-for="pack in store.plugins.packs"
        :key="pack.id"
        class="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden"
      >
        <div class="px-4 py-3 flex items-start gap-3">
          <div class="flex-1 min-w-0">
            <div class="flex items-center gap-2 flex-wrap">
              <span class="font-semibold text-gray-800 dark:text-gray-100">{{ pack.name }}</span>
              <span class="font-mono text-[10px] text-gray-400 dark:text-gray-500">{{
                pack.id
              }}</span>
              <span
                v-if="pack.version"
                class="text-[10px] text-gray-400 dark:text-gray-500"
              >v{{ pack.version }}</span>
              <span
                class="px-1.5 py-0.5 text-[10px] font-semibold rounded bg-gray-100 dark:bg-gray-700 text-gray-600 dark:text-gray-300"
                :title="originPath(pack) ?? 'Shipped with the app'"
              >
                {{ originText(pack) }}
              </span>
            </div>
            <p
              v-if="pack.description"
              class="mt-1 text-xs text-gray-500 dark:text-gray-400"
            >
              {{ pack.description }}
            </p>
            <div
              class="mt-1.5 flex items-center gap-3 text-[11px] text-gray-400 dark:text-gray-500 flex-wrap"
            >
              <span>{{ SOURCE_LABELS[pack.sourceType] ?? pack.sourceType }}</span>
              <span>{{ DISCOVERY_LABELS[pack.discoveryType] ?? pack.discoveryType }}</span>
              <span>{{ pack.environments.length }} environment(s)</span>
              <span>{{ pack.locations.length }} log location(s)</span>
              <span>{{ pack.fields.length }} field(s)</span>
              <span>{{ serviceCounts[pack.id] ?? 0 }} service(s)</span>
              <button
                class="text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 hover:underline"
                @click="toggleDetails(pack.id)"
              >
                {{ expanded.has(pack.id) ? 'hide details' : 'details' }}
              </button>
            </div>
          </div>
          <button
            class="px-3 py-1 text-xs font-medium border border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 rounded-md transition-colors whitespace-nowrap"
            :disabled="busy"
            title="A disabled plugin is not loaded at all; its services stay in the config"
            @click="toggle(pack.id, false)"
          >
            Disable
          </button>
        </div>

        <div
          v-if="expanded.has(pack.id)"
          class="px-4 py-3 border-t border-gray-200 dark:border-gray-700 grid gap-4 md:grid-cols-3 text-xs"
        >
          <div>
            <div class="font-semibold text-gray-600 dark:text-gray-300 mb-1">
              Log locations
            </div>
            <div
              v-for="location in pack.locations"
              :key="location.id"
              class="text-gray-500 dark:text-gray-400"
            >
              {{ location.label }}
              <span class="text-gray-400 dark:text-gray-500 font-mono">({{ location.dir }})</span>
              <span
                v-if="!location.dated"
                class="text-gray-400 dark:text-gray-500"
              >- undated</span>
            </div>
            <div
              v-if="!pack.locations.length"
              class="text-gray-400 dark:text-gray-500"
            >
              none
            </div>
          </div>
          <div>
            <div class="font-semibold text-gray-600 dark:text-gray-300 mb-1">
              Fields
            </div>
            <div
              v-for="field in pack.fields"
              :key="field.key"
              class="text-gray-500 dark:text-gray-400"
            >
              {{ field.label }}
              <span class="text-gray-400 dark:text-gray-500">({{ field.type }})</span>
            </div>
            <div
              v-if="!pack.fields.length"
              class="text-gray-400 dark:text-gray-500"
            >
              none
            </div>
          </div>
          <div>
            <div class="font-semibold text-gray-600 dark:text-gray-300 mb-1">
              Presets and charts
            </div>
            <div
              v-for="preset in pack.presets"
              :key="preset.name"
              class="text-gray-500 dark:text-gray-400 truncate"
              :title="preset.query"
            >
              {{ preset.name }}
            </div>
            <div
              v-for="chart in pack.charts"
              :key="chart.name"
              class="text-gray-500 dark:text-gray-400 truncate"
            >
              {{ chart.name }}
            </div>
            <div
              v-if="!pack.presets.length && !pack.charts.length"
              class="text-gray-400 dark:text-gray-500"
            >
              none
            </div>
          </div>
        </div>
      </div>

      <!-- Disabled bundled plugins can be switched back on; a disabled user file
           simply stops being listed, so only the bundled ones are offered here. -->
      <div
        v-for="id in disabledBundled"
        :key="id"
        class="bg-white dark:bg-gray-800 border border-dashed border-gray-300 dark:border-gray-600 rounded-lg px-4 py-3 flex items-center gap-3 opacity-70"
      >
        <span class="font-mono text-xs text-gray-500 dark:text-gray-400">{{ id }}</span>
        <span class="text-xs text-gray-400 dark:text-gray-500">disabled</span>
        <button
          class="ml-auto px-3 py-1 text-xs font-medium border border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 rounded-md transition-colors"
          :disabled="busy"
          @click="toggle(id, true)"
        >
          Enable
        </button>
      </div>

      <div
        v-if="!store.plugins.packs.length && !disabledBundled.length"
        class="px-4 py-8 text-center text-sm text-gray-400 dark:text-gray-500"
      >
        No plugins loaded. Drop a pack JSON into the plugins folder and reload.
      </div>
    </div>
  </div>
</template>
