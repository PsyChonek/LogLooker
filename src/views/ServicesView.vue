<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from 'vue';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import BaseCheckbox from '@/components/BaseCheckbox.vue';
import ConfirmDialog from '@/components/ConfirmDialog.vue';
import DateRangePicker from '@/components/DateRangePicker.vue';
import Spinner from '@/components/Spinner.vue';
import { formatLogTime } from '@/composables/useTimeMode';
import { formatBytes, useAppStore } from '@/stores/appStore';
import { parseAuthError } from '@/utils/authError';
import type { CacheStatus, Environment, ServiceConfig, SyncProgress, SyncSummary } from '@/types';

const store = useAppStore();
const error = ref<string | null>(null);
const refreshing = ref(false);
const detecting = ref<string | null>(null);
const lastSummaries = ref<SyncSummary[]>([]);
const adding = ref(false);
const addForm = reactive({ name: '', environment: 'test' as Environment, kuduUrl: '' });
const pendingDelete = ref<ServiceConfig | null>(null);
let unlisten: UnlistenFn | null = null;
let unlistenStatus: UnlistenFn | null = null;

const profileLabels: Record<string, string> = {
  'core-applogs': 'applogs (daily)',
  'framework-app-data': 'App_Data/Log.txt',
  'docker-logfiles': 'docker stdout',
};

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

async function refresh() {
  refreshing.value = true;
  error.value = null;
  try {
    await store.refreshServices();
  } catch (e) {
    error.value = String(e);
  } finally {
    refreshing.value = false;
  }
}

async function detect(serviceId: string) {
  detecting.value = serviceId;
  error.value = null;
  store.authNotice = null;
  try {
    await store.detectProfile(serviceId);
  } catch (e) {
    error.value = store.reportError(e);
  } finally {
    detecting.value = null;
  }
}

function kuduShort(url: string): string {
  return url.replace('https://', '').replace('.scm.azurewebsites.net', '');
}

async function editKudu(service: { id: string; kuduUrl: string | null }) {
  const url = prompt(
    'Kudu URL (https://<app>.scm.azurewebsites.net):',
    service.kuduUrl ?? 'https://Example-.scm.azurewebsites.net',
  );
  if (url === null) return;
  error.value = null;
  try {
    await store.setKuduUrl(service.id, url.trim());
  } catch (e) {
    error.value = String(e);
  }
}

function openAddForm() {
  addForm.name = '';
  addForm.environment = store.environment;
  addForm.kuduUrl = '';
  adding.value = true;
}

async function submitAdd() {
  error.value = null;
  try {
    await store.addService(addForm.name, addForm.environment, addForm.kuduUrl);
    store.environment = addForm.environment;
    adding.value = false;
  } catch (e) {
    error.value = String(e);
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
  <div>
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

    <div class="flex items-center gap-4 mb-4">
      <div class="flex rounded-md overflow-hidden border border-gray-300 dark:border-gray-600">
        <button
          v-for="env in ['test', 'production'] as const"
          :key="env"
          class="px-4 py-1.5 text-xs font-semibold uppercase transition-colors"
          :class="
            store.environment === env
              ? env === 'production'
                ? 'bg-red-500 text-white'
                : 'bg-blue-500 text-white'
              : 'bg-white dark:bg-gray-800 text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700'
          "
          @click="setEnvironment(env)"
        >
          {{ env }}
        </button>
      </div>

      <button
        class="px-3 py-1.5 text-xs font-medium border border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 rounded-md transition-colors"
        :disabled="refreshing"
        @click="refresh"
      >
        {{ refreshing ? 'Refreshing...' : 'Refresh from status.example.com' }}
      </button>

      <button
        class="px-3 py-1.5 text-xs font-medium border border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 rounded-md transition-colors"
        @click="openAddForm"
      >
        Add service
      </button>

      <div class="ml-auto flex items-center gap-2">
        <DateRangePicker
          v-model:from="store.dateFrom"
          v-model:to="store.dateTo"
        />
        <button
          class="px-4 py-1.5 text-xs font-semibold rounded-md bg-blue-500 text-white hover:bg-blue-600 disabled:opacity-50 transition-colors"
          :disabled="store.syncing || store.selectedServices.length === 0"
          @click="sync"
        >
          {{ store.syncing ? 'Syncing...' : `Sync ${store.selectedServices.length} selected` }}
        </button>
      </div>
    </div>

    <div
      v-if="store.syncing"
      class="mb-4 px-4 py-2 rounded-md bg-blue-50 dark:bg-blue-900/20 border border-blue-200 dark:border-blue-800 text-xs text-blue-800 dark:text-blue-300 flex items-center gap-3"
    >
      <Spinner size="sm" />
      <template v-if="activeProgress">
        <span class="font-semibold">
          {{ serviceNames[activeProgress.serviceId] ?? activeProgress.serviceId }}
        </span>
        <span>
          service {{ syncPosition }}/{{ syncTotal }} — file
          {{ activeProgress.fileIndex }}/{{ activeProgress.fileCount }}
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
            / {{ formatBytes(activeProgress.totalBytes) }} ({{ syncPercent(activeProgress) }}% of service)
          </template>
        </span>
        <span v-else>{{ activeProgress.state }}</span>
      </template>
      <span v-else>Authenticating and listing remote files...</span>
    </div>

    <form
      v-if="adding"
      class="mb-4 p-3 flex items-end gap-3 rounded-lg bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700"
      @submit.prevent="submitAdd"
    >
      <label class="flex flex-col gap-1 text-xs text-gray-500 dark:text-gray-400">
        Name
        <input
          v-model="addForm.name"
          required
          placeholder="WebJobs"
          class="px-2 py-1 w-48 rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-800 dark:text-gray-100"
        >
      </label>
      <label class="flex flex-col gap-1 text-xs text-gray-500 dark:text-gray-400">
        Environment
        <select
          v-model="addForm.environment"
          class="px-2 py-1 rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-800 dark:text-gray-100"
        >
          <option value="test">
            test
          </option>
          <option value="production">
            production
          </option>
        </select>
      </label>
      <label class="flex flex-col gap-1 flex-1 text-xs text-gray-500 dark:text-gray-400">
        Kudu URL
        <input
          v-model="addForm.kuduUrl"
          required
          placeholder="https://Example-test-webjobs.scm.azurewebsites.net"
          class="px-2 py-1 rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-800 text-gray-800 dark:text-gray-100"
        >
      </label>
      <button
        type="submit"
        class="px-4 py-1.5 text-xs font-semibold rounded-md bg-blue-500 text-white hover:bg-blue-600 transition-colors"
      >
        Add
      </button>
      <button
        type="button"
        class="px-3 py-1.5 text-xs font-medium border border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 rounded-md transition-colors"
        @click="adding = false"
      >
        Cancel
      </button>
    </form>

    <div
      class="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden"
    >
      <div class="overflow-x-auto">
        <table class="w-full min-w-[64rem] text-xs">
          <thead>
            <tr class="border-b border-gray-200 dark:border-gray-700 text-left">
              <th class="py-2 px-3 w-8">
                <BaseCheckbox
                  :model-value="store.allSelected"
                  :indeterminate="store.someSelected && !store.allSelected"
                  :disabled="store.selectableServices.length === 0"
                  :title="store.allSelected ? 'Deselect all' : 'Select all'"
                  @update:model-value="store.setAllSelected($event)"
                />
              </th>
              <th class="py-2 px-3 font-medium text-gray-500 dark:text-gray-400">
                Service
              </th>
              <th class="py-2 px-3 font-medium text-gray-500 dark:text-gray-400">
                Kudu
              </th>
              <th class="py-2 px-3 font-medium text-gray-500 dark:text-gray-400">
                Log profile
              </th>
              <th class="py-2 px-3 font-medium text-gray-500 dark:text-gray-400">
                Cached
              </th>
              <th class="py-2 px-3 font-medium text-gray-500 dark:text-gray-400">
                Coverage
              </th>
              <th class="py-2 px-3 font-medium text-gray-500 dark:text-gray-400">
                Progress
              </th>
              <th class="py-2 px-3 w-8" />
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="service in store.services"
              :key="service.id"
              class="border-b border-gray-100 dark:border-gray-800 hover:bg-gray-50 dark:hover:bg-gray-700/50"
              :class="service.kuduUrl ? 'cursor-pointer' : 'opacity-70'"
              @click="service.kuduUrl && store.toggleSelected(service.id)"
            >
              <td class="py-2 px-3">
                <BaseCheckbox
                  :model-value="store.selectedIds.has(service.id)"
                  :disabled="!service.kuduUrl"
                  class="pointer-events-none"
                />
              </td>
              <td class="py-2 px-3 font-medium text-gray-800 dark:text-gray-100 whitespace-nowrap">
                {{ service.name }}
              </td>
              <td class="py-2 px-3 whitespace-nowrap">
                <template v-if="service.kuduUrl">
                  <span
                    class="text-gray-500 dark:text-gray-400"
                    :title="service.kuduUrl"
                  >
                    {{ kuduShort(service.kuduUrl) }}
                  </span>
                  <button
                    class="ml-2 text-[10px] text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 hover:underline"
                    @click.stop="editKudu(service)"
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
                    @click.stop="editKudu(service)"
                  >
                    Set URL
                  </button>
                </template>
              </td>
              <td class="py-2 px-3 whitespace-nowrap">
                <span
                  v-if="service.profile"
                  class="inline-block px-1.5 py-0.5 text-[10px] font-semibold rounded bg-green-100 dark:bg-green-900/30 text-green-700 dark:text-green-400"
                >
                  {{ profileLabels[service.profile] ?? service.profile }}
                </span>
                <button
                  v-else
                  class="px-2 py-0.5 text-[10px] font-medium border border-gray-300 dark:border-gray-600 text-gray-500 dark:text-gray-400 rounded hover:text-gray-800 dark:hover:text-gray-200"
                  :disabled="detecting === service.id || !service.kuduUrl"
                  @click.stop="detect(service.id)"
                >
                  {{ detecting === service.id ? 'Detecting...' : 'Detect' }}
                </button>
              </td>
              <td class="py-2 px-3 text-gray-500 dark:text-gray-400 whitespace-nowrap">
                <template v-if="store.cacheStatus[service.id]?.files">
                  {{ store.cacheStatus[service.id].files }} files,
                  {{ formatBytes(store.cacheStatus[service.id].compressedBytes) }}
                  <span class="text-gray-400">({{ formatBytes(store.cacheStatus[service.id].uncompressedBytes) }} raw)</span>
                </template>
                <span
                  v-else
                  class="text-gray-300 dark:text-gray-600"
                >empty</span>
              </td>
              <td class="py-2 px-3 text-gray-500 dark:text-gray-400 whitespace-nowrap">
                <template v-if="coverage(service.id)">
                  {{ coverage(service.id)!.from }} → {{ coverage(service.id)!.to }}
                  <span
                    v-if="coverage(service.id)!.zone"
                    class="text-gray-400 dark:text-gray-500"
                  >({{ coverage(service.id)!.zone }})</span>
                </template>
              </td>
              <td class="py-2 px-3 text-gray-500 dark:text-gray-400 whitespace-nowrap">
                <template v-if="summaryFor(service.id)">
                  <span
                    v-if="syncFailed(summaryFor(service.id)!)"
                    class="text-red-600 dark:text-red-400 font-medium"
                  >failed — skipped</span>
                  <template v-else>
                    <span class="text-green-600 dark:text-green-400 font-medium">done</span>
                    — {{ summaryFor(service.id)!.filesDownloaded }} downloaded,
                    {{ summaryFor(service.id)!.filesSkipped }} skipped,
                    {{ formatBytes(summaryFor(service.id)!.bytesDownloaded) }}
                  </template>
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
                      >{{ store.syncProgress[service.id].fileName }}</span>
                      <template v-if="store.syncProgress[service.id].state === 'downloading'">
                        — {{ formatBytes(store.syncProgress[service.id].bytesDownloaded) }}
                        <template v-if="store.syncProgress[service.id].totalBytes">
                          / {{ formatBytes(store.syncProgress[service.id].totalBytes) }}
                        </template>
                      </template>
                      <template v-else>
                        ({{ store.syncProgress[service.id].state }})
                      </template>
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
              </td>
              <td class="py-2 px-3 text-right">
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
              </td>
            </tr>
            <tr v-if="store.services.length === 0">
              <td
                colspan="8"
                class="py-8 text-center text-gray-400"
              >
                No services yet. Use "Refresh from status.example.com" or "Add service".
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>

    <div
      v-for="summary in lastSummaries.filter((s) => s.warnings.length)"
      :key="summary.serviceId"
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
