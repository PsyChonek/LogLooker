<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from 'vue';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import BaseCheckbox from '@/components/BaseCheckbox.vue';
import BaseSelect, { type SelectOption } from '@/components/BaseSelect.vue';
import ConfirmDialog from '@/components/ConfirmDialog.vue';
import DataTable, { type DataTableColumn, type SortState } from '@/components/DataTable.vue';
import DateRangePicker from '@/components/DateRangePicker.vue';
import Spinner from '@/components/Spinner.vue';
import { formatLogTime } from '@/composables/useTimeMode';
import { formatBytes, useAppStore } from '@/stores/appStore';
import { parseAuthError } from '@/utils/authError';
import { sortRows, type SortAccessor } from '@/utils/tableSort';
import type {
  CacheStatus,
  DownloadSummary,
  Environment,
  ServiceConfig,
  SyncProgress,
  SyncSummary,
} from '@/types';

const store = useAppStore();
const error = ref<string | null>(null);
const refreshing = ref(false);
const detecting = ref<string | null>(null);
const lastSummaries = ref<SyncSummary[]>([]);
const lastDownloads = ref<DownloadSummary[]>([]);
const downloadRoot = ref<string | null>(null);
const adding = ref(false);
const addForm = reactive({ name: '', environment: '' as Environment, packId: '', endpoint: '' });
// The add dialog reports its own failures, so a rejected add stays next to the
// form the user is still filling in
const addError = ref<string | null>(null);
const pendingDelete = ref<ServiceConfig | null>(null);
let unlisten: UnlistenFn | null = null;
let unlistenStatus: UnlistenFn | null = null;

/** The label a plugin gives one of its log locations, falling back to its id. */
function locationLabel(service: ServiceConfig): string {
  const location = store.pack(service.packId)?.locations.find((l) => l.id === service.location);
  return location?.label ?? service.location ?? '';
}

/** The plugin's log locations, with re-running detection offered below them. */
function locationOptions(service: ServiceConfig): SelectOption<string>[] {
  return [
    ...(store.pack(service.packId)?.locations ?? []).map((l) => ({ value: l.id, label: l.label })),
    { value: '', label: 'Detect again', separated: true },
  ];
}

const pluginOptions = computed<SelectOption<string>[]>(() =>
  store.plugins.packs.map((pack) => ({ value: pack.id, label: pack.name })),
);

const environmentOptions = computed<SelectOption<string>[]>(() =>
  store.environments.map((env) => ({ value: env.id, label: env.label })),
);

/** What a service's endpoint is called, which depends on its plugin's transport. */
function endpointLabel(packId: string): string {
  return store.pack(packId)?.sourceType === 'local-folder' ? 'Folder' : 'Kudu URL';
}

const columns: DataTableColumn[] = [
  { key: 'select', label: '', width: 48, reorderable: false, resizable: false, hideable: false },
  { key: 'service', label: 'Service', width: 170, sortable: true },
  { key: 'plugin', label: 'Plugin', width: 130, sortable: true },
  { key: 'endpoint', label: 'Endpoint', width: 190, sortable: true },
  { key: 'location', label: 'Log location', width: 170, sortable: true },
  { key: 'cached', label: 'Cached', width: 250, sortable: true },
  { key: 'coverage', label: 'Coverage', width: 360, sortable: true },
  // Flexible: absorbs the leftover table width, so the fixed columns keep
  // their exact widths on wide windows
  { key: 'progress', label: 'Progress', minWidth: 200 },
  {
    key: 'actions',
    label: '',
    width: 44,
    align: 'center',
    reorderable: false,
    resizable: false,
    hideable: false,
  },
];

const sort = ref<SortState | null>(null);

// Sort on raw values (bytes, ISO timestamps), not the formatted cell text
const sortAccessors: Record<string, SortAccessor<ServiceConfig>> = {
  service: (service) => service.name,
  plugin: (service) => service.packId,
  endpoint: (service) => service.endpoint,
  location: (service) => service.location,
  cached: (service) => store.cacheStatus[service.id]?.uncompressedBytes ?? null,
  coverage: (service) => store.cacheStatus[service.id]?.oldest ?? null,
};

const sortedServices = computed(() => sortRows(store.services, sort.value, sortAccessors));

function setEnvironment(env: Environment) {
  store.environment = env;
}

/** The span of log lines cached for a service, read on the clock the Search tab is set to. */
function coverage(serviceId: string) {
  const status = store.cacheStatus[serviceId];
  if (!status?.oldest) return null;
  const offset = store.logOffset(serviceId);
  const from = formatLogTime(status.oldest, offset);
  const to = formatLogTime(status.newest, offset);
  return { from: from.text, to: to.text, zone: to.zone };
}

/** Tooltip for the coverage warning: one missing span per line, on the same clock. */
function gapTooltip(serviceId: string): string | null {
  const gaps = store.cacheStatus[serviceId]?.gaps;
  if (!gaps?.length) return null;
  const offset = store.logOffset(serviceId);
  const lines = gaps.map((gap) => {
    const from = formatLogTime(gap.from, offset);
    const to = formatLogTime(gap.to, offset);
    return `${from.text} → ${to.text}`;
  });
  return `Nothing cached between:\n${lines.join('\n')}`;
}

/** Runs the discovery of one plugin, or of every plugin that has any. */
async function refresh(packId?: string) {
  const packs = packId ? [packId] : store.discoverablePacks.map((p) => p.id);
  if (packs.length === 0) return;
  refreshing.value = true;
  error.value = null;
  const failures: string[] = [];
  try {
    // One plugin's discovery failing (a status page that moved, say) must not
    // stop the others from refreshing
    for (const id of packs) {
      try {
        await store.refreshServices(id);
      } catch (e) {
        failures.push(String(e));
      }
    }
    if (failures.length) error.value = failures.join('\n');
  } finally {
    refreshing.value = false;
  }
}

async function detect(serviceId: string) {
  detecting.value = serviceId;
  error.value = null;
  store.authNotice = null;
  try {
    await store.detectLocation(serviceId);
  } catch (e) {
    error.value = store.reportError(e);
  } finally {
    detecting.value = null;
  }
}

/** Endpoints are long and mostly boilerplate; the host name identifies them. */
function endpointShort(endpoint: string): string {
  return endpoint.replace('https://', '').replace('.scm.azurewebsites.net', '');
}

async function editEndpoint(service: ServiceConfig) {
  const local = store.pack(service.packId)?.sourceType === 'local-folder';
  const value = prompt(
    local ? 'Folder holding the logs:' : 'Kudu URL (https://<app>.scm.azurewebsites.net):',
    service.endpoint ?? (local ? '' : 'https://.scm.azurewebsites.net'),
  );
  if (value === null) return;
  error.value = null;
  try {
    await store.setEndpoint(service.id, value.trim());
  } catch (e) {
    error.value = String(e);
  }
}

/** Overrides which of the plugin's log locations a service reads. */
async function chooseLocation(service: ServiceConfig, location: string) {
  error.value = null;
  try {
    await store.setLocation(service.id, location);
  } catch (e) {
    error.value = String(e);
  }
}

function openAddForm() {
  addForm.name = '';
  addForm.environment = store.environment;
  // Preselect a plugin that knows the environment being looked at; any plugin
  // can be picked afterwards, this is only the likeliest one
  addForm.packId =
    store.plugins.packs.find((p) => p.environments.some((e) => e.id === store.environment))?.id ??
    store.plugins.packs[0]?.id ??
    '';
  addForm.endpoint = '';
  addError.value = null;
  adding.value = true;
}

// A local-folder service points at a path, so the dialog can offer the folder
// picker instead of leaving the user to type it
const addLocal = computed(() => store.pack(addForm.packId)?.sourceType === 'local-folder');

const endpointPlaceholder = computed(() =>
  addLocal.value ? 'C:\\logs\\my-app' : 'https://my-app.scm.azurewebsites.net',
);

/** Picks the folder for a local-folder service, rather than typing the path. */
async function browseFolder() {
  const folder = await open({
    directory: true,
    multiple: false,
    title: 'Choose the folder holding the logs',
  });
  if (typeof folder === 'string') addForm.endpoint = folder;
}

async function submitAdd() {
  addError.value = null;
  try {
    await store.addService(addForm.name, addForm.environment, addForm.packId, addForm.endpoint);
    store.environment = addForm.environment;
    adding.value = false;
  } catch (e) {
    addError.value = String(e);
  }
}

// --- Environments -----------------------------------------------------------

const managingEnvs = ref(false);
const newEnvName = ref('');
const envError = ref<string | null>(null);

function openEnvManager() {
  newEnvName.value = '';
  envError.value = null;
  managingEnvs.value = true;
}

async function submitEnvironment() {
  envError.value = null;
  try {
    await store.addEnvironment(newEnvName.value);
    newEnvName.value = '';
  } catch (e) {
    envError.value = String(e);
  }
}

async function removeEnvironment(environmentId: string) {
  envError.value = null;
  try {
    await store.removeEnvironment(environmentId);
  } catch (e) {
    envError.value = String(e);
  }
}

async function confirmDelete() {
  const service = pendingDelete.value;
  if (!service) return;
  error.value = null;
  try {
    await store.removeService(service.id);
  } catch (e) {
    error.value = String(e);
  } finally {
    pendingDelete.value = null;
  }
}

// Services sync one after another; the banner and per-row spinner follow the
// service the latest progress event came from
const activeSyncId = ref<string | null>(null);
const syncTotal = ref(0);

const activeProgress = computed(() =>
  activeSyncId.value ? store.syncProgress[activeSyncId.value] : null,
);
// syncProgress is reset per sync run, so its key count is the 1-based position
// of the service currently syncing
const syncPosition = computed(() => Object.keys(store.syncProgress).length);

const serviceNames = computed(() =>
  Object.fromEntries(store.config.services.map((s) => [s.id, s.name])),
);

function summaryFor(serviceId: string): SyncSummary | undefined {
  return lastSummaries.value.find((s) => s.serviceId === serviceId);
}

// A service whose sync failed outright: nothing synced, only a warning.
// Its files were skipped so the batch could continue with the other services.
function syncFailed(summary: SyncSummary): boolean {
  return (
    summary.filesDownloaded === 0 &&
    summary.filesSkipped === 0 &&
    summary.filesTotal === 0 &&
    summary.warnings.length > 0
  );
}

// Overall progress across the service's files: finished files count fully,
// the file being downloaded counts by its byte fraction
function syncPercent(p: SyncProgress): number {
  if (p.fileCount === 0) return 0;
  const fileFraction =
    p.state === 'downloading' && p.totalBytes > 0
      ? Math.min(p.bytesDownloaded / p.totalBytes, 1)
      : 1;
  return Math.round(((p.fileIndex - 1 + fileFraction) / p.fileCount) * 100);
}

async function sync() {
  error.value = null;
  store.authNotice = null;
  lastSummaries.value = [];
  activeSyncId.value = null;
  syncTotal.value = store.selectedServices.length;
  try {
    lastSummaries.value = await store.syncSelected();
    // A per-service access failure comes back as a warning, not a thrown error,
    // so promote the first auth-related one to the prominent banner
    const authWarning = lastSummaries.value
      .flatMap((s) => s.warnings)
      .find((w) => parseAuthError(w));
    if (authWarning) store.reportError(authWarning);
  } catch (e) {
    error.value = store.reportError(e);
  } finally {
    activeSyncId.value = null;
  }
}

async function download() {
  const folder = await open({
    directory: true,
    multiple: false,
    title: 'Choose a folder to download the selected logs into',
  });
  if (typeof folder !== 'string') return;

  error.value = null;
  store.authNotice = null;
  lastSummaries.value = [];
  lastDownloads.value = [];
  downloadRoot.value = folder;
  activeSyncId.value = null;
  syncTotal.value = store.selectedServices.length;
  try {
    lastDownloads.value = await store.downloadSelected(folder);
    const authWarning = lastDownloads.value
      .flatMap((s) => s.warnings)
      .find((w) => parseAuthError(w));
    if (authWarning) store.reportError(authWarning);
  } catch (e) {
    error.value = store.reportError(e);
  } finally {
    activeSyncId.value = null;
  }
}

// Totals across the last download, for the result banner
const downloadTotals = computed(() => ({
  files: lastDownloads.value.reduce((sum, s) => sum + s.filesExported, 0),
  bytes: lastDownloads.value.reduce((sum, s) => sum + s.bytesExported, 0),
  failed: lastDownloads.value.reduce((sum, s) => sum + s.filesFailed, 0),
}));

onMounted(async () => {
  unlisten = await listen<SyncProgress>('sync-progress', (event) => {
    store.syncProgress[event.payload.serviceId] = event.payload;
    activeSyncId.value = event.payload.serviceId;
  });
  // Cached size and coverage update per finished file, while the sync runs
  unlistenStatus = await listen<CacheStatus>('cache-status', (event) => {
    store.cacheStatus[event.payload.serviceId] = event.payload;
  });
  try {
    await store.loadConfig();
  } catch (e) {
    error.value = String(e);
  }
});

onUnmounted(() => {
  unlisten?.();
  unlistenStatus?.();
});
</script>

<template>
  <div class="min-w-0 max-w-full">
    <div
      v-if="error"
      class="mb-4 px-4 py-2 rounded-md bg-red-50 dark:bg-red-900/30 border border-red-200 dark:border-red-800 text-sm text-red-700 dark:text-red-400 flex items-center justify-between"
    >
      <span class="break-all">{{ error }}</span>
      <button
        class="ml-4 shrink-0 hover:underline"
        @click="error = null"
      >
        Dismiss
      </button>
    </div>

    <div class="mb-4 flex flex-wrap items-center gap-4">
      <div class="flex rounded-md overflow-hidden border border-gray-300 dark:border-gray-600">
        <button
          v-for="env in store.environments"
          :key="env.id"
          class="px-4 py-1.5 text-xs font-semibold uppercase transition-colors"
          :class="
            store.environment === env.id
              ? env.id.includes('prod')
                ? 'bg-red-500 text-white'
                : 'bg-blue-500 text-white'
              : 'bg-white dark:bg-gray-800 text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700'
          "
          @click="setEnvironment(env.id)"
        >
          {{ env.label }}
        </button>
        <button
          class="px-2.5 py-1.5 text-xs font-semibold border-l border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-500 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 transition-colors"
          title="Add or remove environments"
          @click="openEnvManager"
        >
          +
        </button>
      </div>

      <!-- One button per plugin that can discover services; the plugin's own label
           says where it looks. Plugins without discovery contribute no button. -->
      <button
        v-for="discoverable in store.discoverablePacks"
        :key="discoverable.id"
        class="px-3 py-1.5 text-xs font-medium border border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 rounded-md transition-colors"
        :disabled="refreshing"
        :title="`Discovery of ${discoverable.name}`"
        @click="refresh(discoverable.id)"
      >
        {{
          refreshing
            ? 'Refreshing...'
            : (discoverable.discoveryLabel ?? `Refresh ${discoverable.name}`)
        }}
      </button>

      <button
        class="px-3 py-1.5 text-xs font-medium border border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 rounded-md transition-colors"
        @click="openAddForm"
      >
        Add service
      </button>

      <div class="ml-auto flex min-w-0 flex-wrap items-center justify-end gap-2">
        <DateRangePicker
          v-model:from="store.dateFrom"
          v-model:to="store.dateTo"
          v-model:preset="store.datePreset"
        />
        <button
          class="px-4 py-1.5 text-xs font-semibold rounded-md bg-blue-500 text-white hover:bg-blue-600 disabled:opacity-50 transition-colors"
          :disabled="store.syncing || store.downloading || store.selectedServices.length === 0"
          @click="sync"
        >
          {{ store.syncing ? 'Syncing...' : `Sync ${store.selectedServices.length} selected` }}
        </button>
        <button
          class="px-4 py-1.5 text-xs font-semibold rounded-md border border-blue-500 text-blue-600 dark:text-blue-400 hover:bg-blue-50 dark:hover:bg-blue-900/30 disabled:opacity-50 transition-colors"
          :disabled="store.syncing || store.downloading || store.selectedServices.length === 0"
          title="Download the selected logs, decompressed, into a folder you choose"
          @click="download"
        >
          {{ store.downloading ? 'Downloading...' : 'Download to folder...' }}
        </button>
        <button
          v-if="store.syncing || store.downloading"
          class="px-4 py-1.5 text-xs font-semibold rounded-md border border-red-400 text-red-600 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-900/30 disabled:opacity-50 transition-colors"
          :disabled="store.cancelling"
          title="Stop after the current file; already-downloaded files stay cached"
          @click="store.cancelSync"
        >
          {{ store.cancelling ? 'Cancelling...' : 'Cancel' }}
        </button>
      </div>
    </div>

    <div
      v-if="store.syncing || store.downloading"
      class="mb-4 px-4 py-2 rounded-md bg-blue-50 dark:bg-blue-900/20 border border-blue-200 dark:border-blue-800 text-xs text-blue-800 dark:text-blue-300 flex items-center gap-3"
    >
      <Spinner size="sm" />
      <span
        v-if="store.cancelling"
        class="font-semibold"
      >Cancelling - keeping what was already downloaded...</span>
      <template v-else-if="activeProgress">
        <span class="font-semibold">
          {{ serviceNames[activeProgress.serviceId] ?? activeProgress.serviceId }}
        </span>
        <span>
          service {{ syncPosition }}/{{ syncTotal }} - file {{ activeProgress.fileIndex }}/{{
            activeProgress.fileCount
          }}
        </span>
        <span
          class="font-mono truncate max-w-[20rem]"
          :title="activeProgress.fileName"
        >
          {{ activeProgress.fileName }}
        </span>
        <span v-if="activeProgress.state === 'downloading'">
          {{ formatBytes(activeProgress.bytesDownloaded) }}
          <template v-if="activeProgress.totalBytes">
            / {{ formatBytes(activeProgress.totalBytes) }} ({{ syncPercent(activeProgress) }}% of
            service)
          </template>
        </span>
        <span v-else-if="activeProgress.state === 'processing'">
          {{ formatBytes(activeProgress.bytesDownloaded) }} downloaded - processing...
        </span>
        <span v-else>{{ activeProgress.state }}</span>
      </template>
      <span v-else>Authenticating and listing remote files...</span>
    </div>

    <Teleport to="body">
      <div
        v-if="adding"
        class="fixed inset-0 z-50 flex items-center justify-center bg-black/30 backdrop-blur-sm"
        @click.self="adding = false"
      >
        <form
          class="w-full max-w-lg mx-4 p-6 rounded-lg shadow-xl bg-white dark:bg-gray-800"
          @submit.prevent="submitAdd"
        >
          <h2 class="mb-4 text-base font-semibold text-gray-800 dark:text-gray-100">
            Add service
          </h2>

          <div
            v-if="addError"
            class="mb-4 px-3 py-2 rounded-md bg-red-50 dark:bg-red-900/30 border border-red-200 dark:border-red-800 text-xs text-red-700 dark:text-red-400 break-all"
          >
            {{ addError }}
          </div>

          <div class="flex flex-col gap-3">
            <label class="flex flex-col gap-1 text-xs text-gray-500 dark:text-gray-400">
              Name
              <input
                v-model="addForm.name"
                required
                placeholder="WebJobs"
                class="px-2 py-1 rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-800 dark:text-gray-100"
              >
            </label>
            <div class="flex gap-3">
              <label class="flex flex-col gap-1 flex-1 text-xs text-gray-500 dark:text-gray-400">
                Plugin
                <BaseSelect
                  v-model="addForm.packId"
                  :options="pluginOptions"
                  class="w-full"
                />
              </label>
              <!-- Any environment goes with any plugin, including the local one -->
              <label class="flex flex-col gap-1 flex-1 text-xs text-gray-500 dark:text-gray-400">
                Environment
                <BaseSelect
                  v-model="addForm.environment"
                  :options="environmentOptions"
                  class="w-full"
                />
              </label>
            </div>
            <label class="flex flex-col gap-1 text-xs text-gray-500 dark:text-gray-400">
              {{ endpointLabel(addForm.packId) }}
              <span class="flex gap-2">
                <input
                  v-model="addForm.endpoint"
                  required
                  :placeholder="endpointPlaceholder"
                  class="flex-1 min-w-0 px-2 py-1 rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-800 dark:text-gray-100"
                >
                <button
                  v-if="addLocal"
                  type="button"
                  class="px-3 py-1 text-xs font-medium border border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 rounded-md transition-colors"
                  @click="browseFolder"
                >
                  Browse...
                </button>
              </span>
            </label>
          </div>

          <div class="mt-6 flex justify-end gap-3">
            <button
              type="button"
              class="px-4 py-2 text-sm text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 border border-gray-300 dark:border-gray-600 rounded-md"
              @click="adding = false"
            >
              Cancel
            </button>
            <button
              type="submit"
              class="px-4 py-2 text-sm font-semibold text-white bg-blue-500 hover:bg-blue-600 rounded-md"
            >
              Add
            </button>
          </div>
        </form>
      </div>
    </Teleport>

    <Teleport to="body">
      <div
        v-if="managingEnvs"
        class="fixed inset-0 z-50 flex items-center justify-center bg-black/30 backdrop-blur-sm"
        @click.self="managingEnvs = false"
      >
        <div class="w-full max-w-md mx-4 p-6 rounded-lg shadow-xl bg-white dark:bg-gray-800">
          <h2 class="mb-1 text-base font-semibold text-gray-800 dark:text-gray-100">
            Environments
          </h2>
          <p class="mb-4 text-xs text-gray-500 dark:text-gray-400">
            Name them however you like. Services of any plugin can be put in any of them; the ones
            a plugin declares are fixed.
          </p>

          <div
            v-if="envError"
            class="mb-4 px-3 py-2 rounded-md bg-red-50 dark:bg-red-900/30 border border-red-200 dark:border-red-800 text-xs text-red-700 dark:text-red-400 break-all"
          >
            {{ envError }}
          </div>

          <ul class="mb-4 flex flex-col gap-1 max-h-64 overflow-y-auto">
            <li
              v-for="env in store.environments"
              :key="env.id"
              class="flex items-center gap-2 px-2 py-1 rounded-md text-sm text-gray-700 dark:text-gray-300"
            >
              <span class="font-medium">{{ env.label }}</span>
              <span class="font-mono text-xs text-gray-400">{{ env.id }}</span>
              <button
                v-if="store.customEnvironments.some((e) => e.id === env.id)"
                class="ml-auto text-xs text-red-500 dark:text-red-400 hover:underline"
                title="Remove this environment"
                @click="removeEnvironment(env.id)"
              >
                Remove
              </button>
              <span
                v-else
                class="ml-auto text-[10px] uppercase tracking-wide text-gray-400"
              >from plugin</span>
            </li>
          </ul>

          <form
            class="flex gap-2"
            @submit.prevent="submitEnvironment"
          >
            <input
              v-model="newEnvName"
              required
              placeholder="Staging EU"
              class="flex-1 min-w-0 px-2 py-1 rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-800 dark:text-gray-100"
            >
            <button
              type="submit"
              class="px-4 py-1.5 text-xs font-semibold rounded-md bg-blue-500 text-white hover:bg-blue-600 transition-colors"
            >
              Add
            </button>
          </form>

          <div class="mt-6 flex justify-end">
            <button
              class="px-4 py-2 text-sm text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 border border-gray-300 dark:border-gray-600 rounded-md"
              @click="managingEnvs = false"
            >
              Close
            </button>
          </div>
        </div>
      </div>
    </Teleport>

    <div
      class="min-w-0 max-w-full overflow-hidden rounded-lg border border-gray-200 bg-white dark:border-gray-700 dark:bg-gray-800"
    >
      <DataTable
        v-model:sort="sort"
        table-id="services"
        :columns="columns"
        :rows="sortedServices"
        :row-key="(service) => service.id"
        table-class="min-w-[64rem]"
        scroll-class="max-h-[calc(100vh-180px)]"
        cell-padding="py-2"
        :loading="refreshing"
        clickable-rows
        :row-class="
          (service) => [
            service.endpoint && !store.packMissing(service) ? '' : 'opacity-70',
            store.selectedIds.has(service.id) ? 'bg-blue-50/50 dark:bg-blue-900/10' : '',
          ]
        "
        @row-click="
          (service) =>
            service.endpoint && !store.packMissing(service) && store.toggleSelected(service.id)
        "
      >
        <template #header-select>
          <BaseCheckbox
            :model-value="store.allSelected"
            :indeterminate="store.someSelected && !store.allSelected"
            :disabled="store.selectableServices.length === 0"
            :title="store.allSelected ? 'Deselect all' : 'Select all'"
            @update:model-value="store.setAllSelected($event)"
          />
        </template>

        <template #select="{ row: service }">
          <BaseCheckbox
            :model-value="store.selectedIds.has(service.id)"
            :disabled="!service.endpoint || store.packMissing(service)"
            class="pointer-events-none"
          />
        </template>
        <template #service="{ row: service }">
          <span class="block font-medium text-gray-800 dark:text-gray-100 whitespace-nowrap">
            {{ service.name }}
          </span>
        </template>
        <template #plugin="{ row: service }">
          <div class="whitespace-nowrap">
            <span
              v-if="store.pack(service.packId)"
              class="text-gray-500 dark:text-gray-400"
              :title="store.pack(service.packId)!.description"
            >
              {{ store.pack(service.packId)!.name }}
            </span>
            <!-- The plugin this service belongs to is disabled or gone. The
                 service is kept either way, so say which one is missing. -->
            <span
              v-else
              class="inline-block px-1.5 py-0.5 text-[10px] font-semibold rounded bg-red-100 dark:bg-red-900/30 text-red-700 dark:text-red-400"
              :title="`Plugin &quot;${service.packId}&quot; is not loaded - enable or reinstall it in Plugins`"
            >
              {{ service.packId }} missing
            </span>
          </div>
        </template>
        <template #endpoint="{ row: service }">
          <div class="whitespace-nowrap">
            <template v-if="service.endpoint">
              <span
                class="text-gray-500 dark:text-gray-400"
                :title="service.endpoint"
              >
                {{ endpointShort(service.endpoint) }}
              </span>
              <button
                class="ml-2 text-[10px] text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 hover:underline"
                @click.stop="editEndpoint(service)"
              >
                edit
              </button>
            </template>
            <template v-else>
              <span
                class="inline-block px-1.5 py-0.5 text-[10px] font-semibold rounded bg-amber-100 dark:bg-amber-900/30 text-amber-700 dark:text-amber-400"
              >
                missing
              </span>
              <button
                class="ml-2 px-2 py-0.5 text-[10px] font-medium border border-gray-300 dark:border-gray-600 text-gray-500 dark:text-gray-400 rounded hover:text-gray-800 dark:hover:text-gray-200"
                @click.stop="editEndpoint(service)"
              >
                Set
              </button>
            </template>
          </div>
        </template>
        <template #location="{ row: service }">
          <div class="whitespace-nowrap">
            <!-- Detection is a convenience; the choice stays the user's, so a
                 detected location is still a dropdown over the plugin's list. -->
            <BaseSelect
              v-if="service.location"
              :model-value="service.location"
              :options="locationOptions(service)"
              size="sm"
              tone="positive"
              class="max-w-[10rem]"
              :title="locationLabel(service)"
              @click.stop
              @change="chooseLocation(service, $event)"
            />
            <button
              v-else
              class="px-2 py-0.5 text-[10px] font-medium border border-gray-300 dark:border-gray-600 text-gray-500 dark:text-gray-400 rounded hover:text-gray-800 dark:hover:text-gray-200"
              :disabled="
                detecting === service.id || !service.endpoint || store.packMissing(service)
              "
              @click.stop="detect(service.id)"
            >
              {{ detecting === service.id ? 'Detecting...' : 'Detect' }}
            </button>
          </div>
        </template>
        <template #cached="{ row: service }">
          <div class="text-gray-500 dark:text-gray-400 whitespace-nowrap">
            <template v-if="store.cacheStatus[service.id]?.files">
              {{ store.cacheStatus[service.id].files }} files,
              {{ formatBytes(store.cacheStatus[service.id].compressedBytes) }}
              <span class="text-gray-400">({{ formatBytes(store.cacheStatus[service.id].uncompressedBytes) }} raw)</span>
            </template>
            <span
              v-else
              class="text-gray-300 dark:text-gray-600"
            >empty</span>
          </div>
        </template>
        <template #coverage="{ row: service }">
          <div class="text-gray-500 dark:text-gray-400 whitespace-nowrap">
            <template v-if="coverage(service.id)">
              {{ coverage(service.id)!.from }} → {{ coverage(service.id)!.to }}
              <span
                v-if="coverage(service.id)!.zone"
                class="text-gray-400 dark:text-gray-500"
              >({{ coverage(service.id)!.zone }})</span>
              <span
                v-if="gapTooltip(service.id)"
                class="inline-flex align-text-bottom ml-1 text-amber-500 dark:text-amber-400 cursor-help"
                :title="gapTooltip(service.id)!"
              >
                <svg
                  class="size-4"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.5"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  aria-hidden="true"
                >
                  <path
                    d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0Z"
                  />
                  <path d="M12 9v4M12 17h.01" />
                </svg>
              </span>
            </template>
          </div>
        </template>
        <template #progress="{ row: service }">
          <div class="text-gray-500 dark:text-gray-400 whitespace-nowrap">
            <template v-if="summaryFor(service.id)">
              <span
                v-if="syncFailed(summaryFor(service.id)!)"
                class="text-red-600 dark:text-red-400 font-medium"
              >failed - skipped</span>
              <template v-else>
                <span
                  v-if="summaryFor(service.id)!.filesFailed"
                  class="text-amber-600 dark:text-amber-400 font-medium"
                >incomplete</span>
                <span
                  v-else
                  class="text-green-600 dark:text-green-400 font-medium"
                >done</span>
                - {{ summaryFor(service.id)!.filesDownloaded }} downloaded,
                {{ summaryFor(service.id)!.filesSkipped }} skipped<span
                  v-if="summaryFor(service.id)!.filesFailed"
                  class="text-red-600 dark:text-red-400 font-medium"
                >, {{ summaryFor(service.id)!.filesFailed }} failed</span>
                of {{ summaryFor(service.id)!.filesTotal }},
                {{ formatBytes(summaryFor(service.id)!.bytesDownloaded) }}
              </template>
              <!-- Per-file trouble (failed downloads, interrupted transfers,
                   multi-instance coverage) lands in warnings; without this
                   the run looks clean unless the user opens the log -->
              <span
                v-if="summaryFor(service.id)!.warnings.length"
                class="inline-flex align-text-bottom ml-1 text-amber-500 dark:text-amber-400 cursor-help"
                :title="summaryFor(service.id)!.warnings.join('\n')"
              >
                <svg
                  class="size-4"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.5"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  aria-hidden="true"
                >
                  <path
                    d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0Z"
                  />
                  <path d="M12 9v4M12 17h.01" />
                </svg>
              </span>
            </template>
            <template v-else-if="store.syncProgress[service.id]">
              <div class="flex items-center gap-2">
                <Spinner
                  v-if="store.syncing && activeSyncId === service.id"
                  size="sm"
                />
                <span>
                  file {{ store.syncProgress[service.id].fileIndex }}/{{
                    store.syncProgress[service.id].fileCount
                  }}
                  <span
                    class="font-mono"
                    :title="store.syncProgress[service.id].fileName"
                  >{{
                    store.syncProgress[service.id].fileName
                  }}</span>
                  <template v-if="store.syncProgress[service.id].state === 'downloading'">
                    - {{ formatBytes(store.syncProgress[service.id].bytesDownloaded) }}
                    <template v-if="store.syncProgress[service.id].totalBytes">
                      / {{ formatBytes(store.syncProgress[service.id].totalBytes) }}
                    </template>
                  </template>
                  <template v-else> ({{ store.syncProgress[service.id].state }}) </template>
                </span>
              </div>
              <div
                v-if="store.syncing && activeSyncId === service.id"
                class="mt-1 h-1 w-44 rounded bg-gray-200 dark:bg-gray-700 overflow-hidden"
              >
                <div
                  class="h-full bg-blue-500 transition-all duration-200"
                  :style="{ width: `${syncPercent(store.syncProgress[service.id])}%` }"
                />
              </div>
            </template>
          </div>
        </template>
        <template #actions="{ row: service }">
          <button
            v-if="service.source !== 'scraped'"
            class="inline-flex text-red-500 dark:text-red-400 hover:text-red-600 dark:hover:text-red-300"
            :title="`Remove ${service.name}`"
            @click.stop="pendingDelete = service"
          >
            <svg
              class="size-4"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="M4 7h16" />
              <path d="M10 4h4a1 1 0 0 1 1 1v2H9V5a1 1 0 0 1 1-1Z" />
              <path d="M6 7v12a1 1 0 0 0 1 1h10a1 1 0 0 0 1-1V7" />
              <path d="M10 11v5M14 11v5" />
            </svg>
          </button>
        </template>
        <template #empty>
          No services yet. Use a plugin's refresh button or "Add service".
        </template>
      </DataTable>
    </div>

    <div
      v-for="summary in lastSummaries.filter((s) => s.warnings.length)"
      :key="summary.serviceId"
      class="mt-3 px-4 py-2 rounded-md bg-yellow-50 dark:bg-yellow-900/20 border border-yellow-200 dark:border-yellow-800 text-xs text-yellow-800 dark:text-yellow-300"
    >
      <strong>{{ summary.serviceId }}:</strong> {{ summary.warnings.join('; ') }}
    </div>

    <div
      v-if="lastDownloads.length && !store.downloading"
      class="mt-3 px-4 py-2 rounded-md bg-green-50 dark:bg-green-900/20 border border-green-200 dark:border-green-800 text-xs text-green-800 dark:text-green-300"
    >
      Downloaded <strong>{{ downloadTotals.files }}</strong> file(s),
      {{ formatBytes(downloadTotals.bytes) }} decompressed<span
        v-if="downloadTotals.failed"
        class="text-red-600 dark:text-red-400 font-medium"
      >, {{ downloadTotals.failed }} failed to fetch</span>, into
      <span
        class="font-mono break-all"
        :title="downloadRoot ?? undefined"
      >{{ downloadRoot }}</span>
      <span class="text-green-700/70 dark:text-green-400/70">(one subfolder per service)</span>
    </div>

    <div
      v-for="summary in lastDownloads.filter((s) => s.warnings.length)"
      :key="`dl-${summary.serviceId}`"
      class="mt-3 px-4 py-2 rounded-md bg-yellow-50 dark:bg-yellow-900/20 border border-yellow-200 dark:border-yellow-800 text-xs text-yellow-800 dark:text-yellow-300"
    >
      <strong>{{ summary.serviceId }}:</strong> {{ summary.warnings.join('; ') }}
    </div>

    <ConfirmDialog
      :visible="pendingDelete !== null"
      :message="`Remove ${pendingDelete?.id} from the service list? Logs already cached stay on disk.`"
      @confirm="confirmDelete"
      @cancel="pendingDelete = null"
    />
  </div>
</template>
