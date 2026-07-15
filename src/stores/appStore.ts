import { computed, ref } from 'vue';
import { acceptHMRUpdate, defineStore } from 'pinia';
import { invoke } from '@tauri-apps/api/core';
import { parseAuthError, type AuthNotice } from '@/utils/authError';
import type {
  AppConfig,
  CacheStatus,
  Environment,
  MemoryCacheStats,
  ProfileKind,
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

export const useAppStore = defineStore('app', () => {
  const config = ref<AppConfig>({
    services: [],
    savedQueries: [],
    seededBuiltins: [],
    memoryCache: { enabled: false, maxMb: 2048 },
  });
  const memoryCache = ref<MemoryCacheStats | null>(null);
  const cacheStatus = ref<Record<string, CacheStatus>>({});
  const environment = ref<Environment>('test');
  const selectedIds = ref<Set<string>>(new Set());
  const dateFrom = ref(isoDate(2));
  const dateTo = ref(isoDate(0));
  const syncing = ref(false);
  const syncProgress = ref<Record<string, SyncProgress>>({});
  // App-wide notice for az/access failures, shown in the header area regardless
  // of which action triggered it
  const authNotice = ref<AuthNotice | null>(null);

  const services = computed(() =>
    config.value.services.filter((s) => s.environment === environment.value),
  );
  const selectedServices = computed(() =>
    services.value.filter((s) => selectedIds.value.has(s.id)),
  );
  // Services without a Kudu URL cannot be synced, so they are not selectable
  const selectableServices = computed(() => services.value.filter((s) => s.kuduUrl));
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
    await loadCacheStatus();
  }

  async function saveConfig() {
    await invoke('update_config', { config: config.value });
  }

  async function refreshServices() {
    config.value.services = await invoke<ServiceConfig[]>('refresh_services');
  }

  async function detectProfile(serviceId: string) {
    const profile = await invoke<ProfileKind | null>('detect_profile', { serviceId });
    const service = config.value.services.find((s) => s.id === serviceId);
    if (service) service.profile = profile;
    return profile;
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
    syncing.value = true;
    syncProgress.value = {};
    try {
      return await invoke<SyncSummary[]>('sync_services', {
        serviceIds: [...selectedIds.value].filter((id) => services.value.some((s) => s.id === id)),
        dateFrom: dateFrom.value,
        dateTo: dateTo.value,
      });
    } finally {
      syncing.value = false;
      // Files downloaded before a mid-batch error must still show up
      await loadCacheStatus().catch(() => {});
    }
  }

  async function setKuduUrl(serviceId: string, kuduUrl: string) {
    config.value.services = await invoke<ServiceConfig[]>('set_kudu_url', { serviceId, kuduUrl });
  }

  async function addService(name: string, env: Environment, kuduUrl: string) {
    config.value.services = await invoke<ServiceConfig[]>('add_service', {
      name,
      environment: env,
      kuduUrl,
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
    cacheStatus,
    memoryCache,
    environment,
    selectedIds,
    dateFrom,
    dateTo,
    syncing,
    syncProgress,
    authNotice,
    services,
    selectedServices,
    selectableServices,
    allSelected,
    someSelected,
    loadConfig,
    saveConfig,
    refreshServices,
    detectProfile,
    loadCacheStatus,
    logOffset,
    loadMemoryCache,
    setMemoryCache,
    clearMemoryCache,
    syncSelected,
    setKuduUrl,
    addService,
    removeService,
    toggleSelected,
    setAllSelected,
    reportError,
  };
});

// Without this, editing the store during `tauri dev` leaves components holding
// a stale store instance (e.g. "store.setKuduUrl is not a function")
if (import.meta.hot) {
  import.meta.hot.accept(acceptHMRUpdate(useAppStore, import.meta.hot));
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1048576) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1073741824) return `${(bytes / 1048576).toFixed(1)} MB`;
  return `${(bytes / 1073741824).toFixed(2)} GB`;
}
