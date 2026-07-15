export interface LogEntry {
  timestamp: string;
  level: string;
  source: string;
  message: string;
}

export type Environment = 'test' | 'production';

export type ProfileKind = 'core-applogs' | 'framework-app-data' | 'docker-logfiles';

// 'scraped' services are owned by the status page refresh and cannot be removed
export type ServiceSource = 'scraped' | 'builtin' | 'manual';

export interface ServiceConfig {
  id: string;
  name: string;
  environment: Environment;
  kuduUrl: string | null;
  // True once the user hand-edited the Kudu URL, so a refresh keeps their value
  kuduUrlManual: boolean;
  profile: ProfileKind | null;
  source: ServiceSource;
}

export interface SavedQuery {
  name: string;
  query: string;
  isRegex: boolean;
}

export interface AppConfig {
  services: ServiceConfig[];
  savedQueries: SavedQuery[];
  // Built-in ids already seeded once; must round-trip through update_config,
  // otherwise a deleted built-in comes back on the next start
  seededBuiltins: string[];
  memoryCache: MemoryCacheConfig;
}

export interface MemoryCacheConfig {
  enabled: boolean;
  // Cap on the decompressed log text held in RAM
  maxMb: number;
}

// Live state of the backend cache (src-tauri/src/memcache.rs). It holds the
// decompressed text of the files that fit; the rest is read from disk.
export interface MemoryCacheStats {
  enabled: boolean;
  maxBytes: number;
  usedBytes: number;
  files: number;
}

export interface SyncProgress {
  serviceId: string;
  fileName: string;
  fileIndex: number;
  fileCount: number;
  bytesDownloaded: number;
  totalBytes: number;
  state: 'downloading' | 'done' | 'skipped';
}

export interface SyncSummary {
  serviceId: string;
  filesTotal: number;
  filesDownloaded: number;
  filesSkipped: number;
  bytesDownloaded: number;
  warnings: string[];
}

export interface CacheStatus {
  serviceId: string;
  files: number;
  compressedBytes: number;
  uncompressedBytes: number;
  oldest: string | null;
  newest: string | null;
  // Minutes the service's log clock runs ahead of UTC; null while it cannot be told
  logOffsetMinutes: number | null;
}

export interface SearchRequest {
  serviceIds: string[];
  dateFrom: string;
  dateTo: string;
  query: string;
  isRegex: boolean;
  caseSensitive: boolean;
  contextLines: number;
}

export interface ExtractedFields {
  idLogin: string | null;
  idCommand: string | null;
  operation: string | null;
  durationMs: number | null;
}

export interface SearchHit {
  serviceId: string;
  file: string;
  lineNumber: number;
  timestamp: string | null;
  line: string;
  contextBefore: string[];
  contextAfter: string[];
  fields: ExtractedFields;
  // Named capture groups of the query that matched somewhere in this line
  matchedGroups: string[];
}

// Live progress of a running search; files scan in parallel, so currentFile
// is the most recently finished one, not a strict position
export interface SearchProgress {
  phase: 'scanning' | 'sorting';
  filesDone: number;
  filesTotal: number;
  hits: number;
  linesScanned: number;
  currentFile: string;
}

export interface SearchMeta {
  totalHits: number;
  filesScanned: number;
  linesScanned: number;
  durationMs: number;
  // Named capture groups of the query, in pattern order
  groupNames: string[];
}

// --- Charts (aggregation of the last search, see src-tauri/src/chart.rs) ---

export type ChartMode = 'timeline' | 'category';
export type ChartGroupBy =
  'none' | 'service' | 'operation' | 'idLogin' | 'file' | 'custom' | 'matchedGroup';
export type ChartMetric = 'count' | 'sum' | 'avg' | 'min' | 'max' | 'p50' | 'p95' | 'p99';
export type ChartValueSource = 'duration' | 'custom';
export type ChartBucket =
  'auto' | 'second' | 'minute' | 'fiveMinutes' | 'fifteenMinutes' | 'hour' | 'sixHours' | 'day';

export interface ChartRequest {
  mode: ChartMode;
  bucket: ChartBucket;
  groupBy: ChartGroupBy;
  // Needed by groupBy 'custom' and valueSource 'custom': the category comes from
  // the (?<key>...) group, the number from (?<value>...)
  customRegex: string | null;
  metric: ChartMetric;
  valueSource: ChartValueSource;
  topN: number;
}

export interface ChartSeries {
  name: string;
  // null is a bucket with no data — a gap in a line, no bar in a bar chart
  values: (number | null)[];
}

export interface ChartData {
  labels: string[];
  series: ChartSeries[];
  bucketSeconds: number;
  bucketLabel: string;
  metricLabel: string;
  charted: number;
  skippedNoTime: number;
  skippedNoValue: number;
  skippedNoKey: number;
  otherGroups: number;
  // The clock the bucket labels are cut on; null when the charted services disagree
  logOffsetMinutes: number | null;
}

// Injected by the open_chart_window command into windows that show a chart
export interface ChartViewParams {
  query: string;
  request: ChartRequest;
}

export interface RawFileInfo {
  handle: number;
  serviceId: string;
  file: string;
  totalLines: number;
  sizeBytes: number;
}

export interface RawSearchMatch {
  lineNumber: number;
  text: string;
}

export interface RawSearchResult {
  matches: RawSearchMatch[];
  total: number;
}

// Injected by the open_raw_window command into windows that show a single raw file
export interface RawViewParams {
  serviceId: string;
  serviceName: string;
  file: string;
  line: number | null;
  query: string | null;
  isRegex: boolean;
  caseSensitive: boolean;
}

export interface CachedFileInfo {
  serviceId: string;
  file: string;
  date: string | null;
  sizeBytes: number;
  compressedBytes: number;
  instance: string | null;
}
