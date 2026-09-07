import { computed, ref, watch } from 'vue';
import { acceptHMRUpdate, defineStore } from 'pinia';
import { invoke } from '@tauri-apps/api/core';
import { parseAuthError, type AuthNotice } from '@/utils/authError';
import { resolvePreset, todayKey, type DateRangeSelection, type PresetId } from '@/utils/dateRange';
import type {
  AppConfig,
  CacheStatus,
  DownloadSummary,
  Environment,
  EnvironmentDef,
  FieldInfo,
  MemoryCacheStats,
  PackInfo,
  PluginsInfo,
  ServiceConfig,
  SyncProgress,
  SyncSummary,
} from '@/types';

function isoDate(daysAgo: number): string {
  const date = new Date();
  date.setDate(date.getDate() - daysAgo);
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  return `${date.getFullYear()}-${month}-${day}`;
}

// Remembers the environment, service selection and date range across restarts,
// so the app reopens on whatever the user last looked at. Environment travels
// with the selection because the selectable services are scoped to it.
const STATE_KEY = 'loglooker.searchState';

interface PersistedState {
  environment?: Environment;
  selectedIds?: string[];
  // A preset is stored by id and re-resolved on every read, so a restored
  // "Last 7 days" follows the calendar instead of freezing on the day it was
  // picked. null means the user picked an explicit range (dateFrom/dateTo).
  datePreset?: PresetId | null;
  dateFrom?: string;
  dateTo?: string;
}

const DEFAULT_PRESET: PresetId = 'last3';

function loadState(): PersistedState {
  try {
    return JSON.parse(localStorage.getItem(STATE_KEY) ?? '{}') as PersistedState;
  } catch {
    return {};
  }
}

export const useAppStore = defineStore('app', () => {
  const config = ref<AppConfig>({
    services: [],
    environments: [],
    savedQueries: [],
    seededBuiltins: [],
    disabledPacks: [],
    memoryCache: { enabled: false, maxMb: 2048 },
    search: { maxHits: 2_000_000 },
  });
  // What the loaded plugins offer. Everything the UI can present as a choice -
  // environments, log locations, extracted fields, chart dimensions - comes from
  // here rather than being written into the app.
  const plugins = ref<PluginsInfo>({
    packs: [],
    errors: [],
    disabled: [],
    pluginsDir: '',
    bundled: [],
  });
  const memoryCache = ref<MemoryCacheStats | null>(null);
  const cacheStatus = ref<Record<string, CacheStatus>>({});
  const saved = loadState();
  const environment = ref<Environment>(saved.environment ?? 'test');
  const selectedIds = ref<Set<string>>(new Set(saved.selectedIds ?? []));
  // State saved before presets existed only has concrete dates - keep those as a
  // custom range rather than silently snapping to a preset.
  const datePreset = ref<PresetId | null>(
    saved.datePreset !== undefined ? saved.datePreset : saved.dateFrom ? null : DEFAULT_PRESET,
  );
  const customFrom = ref(saved.dateFrom ?? isoDate(2));
  const customTo = ref(saved.dateTo ?? isoDate(0));

  // Reactive "today" so an active preset re-resolves when the day rolls over
  // while the app stays open (or the machine wakes from sleep).
  const today = ref(todayKey());
  function refreshToday() {
    const key = todayKey();
    if (key !== today.value) today.value = key;
  }
  setInterval(refreshToday, 30_000);
  window.addEventListener('focus', refreshToday);
  document.addEventListener('visibilitychange', refreshToday);

  const activeRange = computed(() => {
    const preset = datePreset.value ? resolvePreset(datePreset.value, today.value) : null;
    return preset ?? { from: customFrom.value, to: customTo.value };
  });

  function setDateRange(selection: DateRangeSelection) {
    customFrom.value = selection.from;
    customTo.value = selection.to;
    refreshToday();
    datePreset.value = selection.preset;
  }

  // Writing either end means the user picked explicit dates, so the preset drops.
  const dateFrom = computed({
    get: () => activeRange.value.from,
    set: (value: string) => {
      customTo.value = activeRange.value.to;
      datePreset.value = null;
      customFrom.value = value;
    },
  });
  const dateTo = computed({
    get: () => activeRange.value.to,
    set: (value: string) => {
      customFrom.value = activeRange.value.from;
      datePreset.value = null;
      customTo.value = value;
    },
  });

  // selectedIds is reassigned to a fresh Set on every change, so a shallow watch
  // catches each mutation without needing deep tracking.
  watch([environment, selectedIds, datePreset, dateFrom, dateTo], () => {
    const state: PersistedState = {
      environment: environment.value,
      selectedIds: [...selectedIds.value],
      datePreset: datePreset.value,
      dateFrom: dateFrom.value,
      dateTo: dateTo.value,
    };
    localStorage.setItem(STATE_KEY, JSON.stringify(state));
  });
  const syncing = ref(false);
  const downloading = ref(false);
  // Set once the user asks to cancel; cleared when the run starts/ends. Drives
  // the "Cancelling..." button state while the backend winds down.
  const cancelling = ref(false);
  const syncProgress = ref<Record<string, SyncProgress>>({});
  // App-wide notice for az/access failures, shown in the header area regardless
  // of which action triggered it
  const authNotice = ref<AuthNotice | null>(null);

  // Union across plugins: one TEST toggle covers every plugin's test services.
  // The user's own environments follow, so the familiar ones stay leftmost.
  const environments = computed<EnvironmentDef[]>(() => {
    const seen = new Map<string, EnvironmentDef>();
    for (const pack of plugins.value.packs) {
      for (const env of pack.environments) {
        if (!seen.has(env.id)) seen.set(env.id, env);
      }
    }
    for (const env of config.value.environments) {
      if (!seen.has(env.id)) seen.set(env.id, env);
    }
    return [...seen.values()];
  });

  // The ones the user added, i.e. the ones that can be removed again
  const customEnvironments = computed(() => config.value.environments);

  async function addEnvironment(name: string) {
    config.value.environments = await invoke<EnvironmentDef[]>('add_environment', { name });
  }

  async function removeEnvironment(environmentId: string) {
    config.value.environments = await invoke<EnvironmentDef[]>('remove_environment', {
      environmentId,
    });
    if (environment.value === environmentId) {
      environment.value = environments.value[0]?.id ?? '';
    }
  }

  const fields = computed<FieldInfo[]>(() => plugins.value.packs.flatMap((p) => p.fields));

  function pack(packId: string): PackInfo | undefined {
    return plugins.value.packs.find((p) => p.id === packId);
  }

  // A service whose plugin is missing cannot sync or search; the Services table
  // says so rather than the service silently doing nothing.
  function packMissing(service: ServiceConfig): boolean {
    return !pack(service.packId);
  }

  const services = computed(() =>
    config.value.services.filter((s) => s.environment === environment.value),
  );
  const selectedServices = computed(() =>
    services.value.filter((s) => selectedIds.value.has(s.id)),
  );
  // A service with no endpoint, or whose plugin is not loaded, cannot be synced,
  // so it is not selectable either
  const selectableServices = computed(() =>
    services.value.filter((s) => s.endpoint && !packMissing(s)),
  );
  const allSelected = computed(
    () =>
      selectableServices.value.length > 0 &&
      selectableServices.value.every((s) => selectedIds.value.has(s.id)),
  );
  const someSelected = computed(() =>
    selectableServices.value.some((s) => selectedIds.value.has(s.id)),
  );

  async function loadConfig() {
    config.value = await invoke<AppConfig>('get_config');
    await loadPlugins();
    await loadCacheStatus();
  }

  async function loadPlugins() {
    plugins.value = await invoke<PluginsInfo>('get_plugins');
    // A saved environment can belong to a plugin that is now gone; fall back to
    // the first one that exists so the app does not open on an empty list
    if (
      environments.value.length > 0 &&
      !environments.value.some((e) => e.id === environment.value)
    ) {
      environment.value = environments.value[0].id;
    }
  }

  async function reloadPlugins() {
    plugins.value = await invoke<PluginsInfo>('reload_plugins');
    // A reload can bind services to a plugin that has only now appeared
    config.value = await invoke<AppConfig>('get_config');
  }

  async function setPackEnabled(packId: string, enabled: boolean) {
    plugins.value = await invoke<PluginsInfo>('set_pack_enabled', { packId, enabled });
    config.value = await invoke<AppConfig>('get_config');
  }

  async function openPluginsDir() {
    await invoke('open_plugins_dir');
  }

  async function saveConfig() {
    await invoke('update_config', { config: config.value });
  }

  // Discovery belongs to one plugin, so a refresh names which
  async function refreshServices(packId: string) {
    config.value.services = await invoke<ServiceConfig[]>('refresh_services', { packId });
  }

  // Plugins that can discover services, i.e. what the refresh button offers
  const discoverablePacks = computed(() =>
    plugins.value.packs.filter((p) => p.discoveryType !== 'none'),
  );

  async function detectLocation(serviceId: string) {
    const location = await invoke<string | null>('detect_location', { serviceId });
    const service = config.value.services.find((s) => s.id === serviceId);
    if (service) service.location = location;
    return location;
  }

  async function setLocation(serviceId: string, location: string) {
    config.value.services = await invoke<ServiceConfig[]>('set_location', {
      serviceId,
      location,
    });
  }

  async function loadCacheStatus() {
    const statuses = await invoke<CacheStatus[]>('cache_status_all');
    cacheStatus.value = Object.fromEntries(statuses.map((s) => [s.serviceId, s]));
  }

  // Detected from what is cached, so a service gains an offset only once it has files
  function logOffset(serviceId: string): number | null {
    return cacheStatus.value[serviceId]?.logOffsetMinutes ?? null;
  }

  // The three memory-cache calls all answer with the state after the change, so
  // config and the live readout never drift apart
  async function loadMemoryCache() {
    memoryCache.value = await invoke<MemoryCacheStats>('memory_cache_stats');
  }

  async function setMemoryCache(enabled: boolean, maxMb: number) {
    const stats = await invoke<MemoryCacheStats>('set_memory_cache', { enabled, maxMb });
    memoryCache.value = stats;
    // The backend clamps the cap; mirror what it actually put in force
    config.value.memoryCache = { enabled: stats.enabled, maxMb: stats.maxBytes / 1048576 };
  }

  async function clearMemoryCache() {
    memoryCache.value = await invoke<MemoryCacheStats>('clear_memory_cache');
  }

  async function syncSelected(): Promise<SyncSummary[]> {
    refreshToday();
    syncing.value = true;
    cancelling.value = false;
    syncProgress.value = {};
    try {
      return await invoke<SyncSummary[]>('sync_services', {
        serviceIds: [...selectedIds.value].filter((id) => services.value.some((s) => s.id === id)),
        dateFrom: dateFrom.value,
        dateTo: dateTo.value,
      });
    } finally {
      syncing.value = false;
      cancelling.value = false;
      // Files downloaded before a mid-batch error must still show up
      await loadCacheStatus().catch(() => {});
    }
  }

  // Signals the running sync/download to stop after the current file; the
  // backend returns the partial summaries normally, so the callers resolve as
  // usual. Shared by both since the UI runs only one at a time.
  async function cancelSync() {
    if ((!syncing.value && !downloading.value) || cancelling.value) return;
    cancelling.value = true;
    await invoke('cancel_sync');
  }

  // Same fetch as sync (only missing/changed files are downloaded), then the
  // cached files for the range are decompressed into a folder the user picks,
  // one subfolder per service. Reuses the sync-progress events for live status.
  async function downloadSelected(targetDir: string): Promise<DownloadSummary[]> {
    refreshToday();
    downloading.value = true;
    cancelling.value = false;
    syncProgress.value = {};
    try {
      return await invoke<DownloadSummary[]>('download_services', {
        serviceIds: [...selectedIds.value].filter((id) => services.value.some((s) => s.id === id)),
        dateFrom: dateFrom.value,
        dateTo: dateTo.value,
        targetDir,
      });
    } finally {
      downloading.value = false;
      cancelling.value = false;
      await loadCacheStatus().catch(() => {});
    }
  }

  async function setEndpoint(serviceId: string, endpoint: string) {
    config.value.services = await invoke<ServiceConfig[]>('set_endpoint', { serviceId, endpoint });
  }

  async function addService(name: string, env: Environment, packId: string, endpoint: string) {
    config.value.services = await invoke<ServiceConfig[]>('add_service', {
      name,
      environment: env,
      packId,
      endpoint,
    });
  }

  async function removeService(serviceId: string) {
    config.value.services = await invoke<ServiceConfig[]>('remove_service', { serviceId });
    selectedIds.value.delete(serviceId);
    selectedIds.value = new Set(selectedIds.value);
  }

  function toggleSelected(serviceId: string) {
    if (selectedIds.value.has(serviceId)) {
      selectedIds.value.delete(serviceId);
    } else {
      selectedIds.value.add(serviceId);
    }
    selectedIds.value = new Set(selectedIds.value);
  }

  // Routes a caught error: az/access failures go to the prominent global banner
  // and return null (so the caller shows nothing inline); anything else is
  // returned as a string for the caller's local error display.
  function reportError(e: unknown): string | null {
    const raw = String(e);
    const notice = parseAuthError(raw);
    if (notice) {
      authNotice.value = notice;
      return null;
    }
    return raw;
  }

  // Only touches the current environment; a selection made in the other one stays intact
  function setAllSelected(selected: boolean) {
    const next = new Set(selectedIds.value);
    for (const service of selectableServices.value) {
      if (selected) next.add(service.id);
      else next.delete(service.id);
    }
    selectedIds.value = next;
  }

  return {
    config,
    plugins,
    environments,
    customEnvironments,
    addEnvironment,
    removeEnvironment,
    discoverablePacks,
    fields,
    pack,
    packMissing,
    cacheStatus,
    memoryCache,
    environment,
    selectedIds,
    datePreset,
    dateFrom,
    dateTo,
    setDateRange,
    syncing,
    downloading,
    cancelling,
    syncProgress,
    authNotice,
    services,
    selectedServices,
    selectableServices,
    allSelected,
    someSelected,
    loadConfig,
    saveConfig,
    loadPlugins,
    reloadPlugins,
    setPackEnabled,
    openPluginsDir,
    refreshServices,
    detectLocation,
    setLocation,
    loadCacheStatus,
    logOffset,
    loadMemoryCache,
    setMemoryCache,
    clearMemoryCache,
    syncSelected,
    cancelSync,
    downloadSelected,
    setEndpoint,
    addService,
    removeService,
    toggleSelected,
    setAllSelected,
    reportError,
  };
});

// Without this, editing the store during `tauri dev` leaves components holding
// a stale store instance (e.g. "store.setEndpoint is not a function")
if (import.meta.hot) {
  import.meta.hot.accept(acceptHMRUpdate(useAppStore, import.meta.hot));
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1048576) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1073741824) return `${(bytes / 1048576).toFixed(1)} MB`;
  return `${(bytes / 1073741824).toFixed(2)} GB`;
}
