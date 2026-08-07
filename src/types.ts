export interface LogEntry {
  timestamp: string;
  level: string;
  source: string;
  message: string;
}

// An environment id as some loaded plugin declares it, e.g. 'test'
export type Environment = string;

export interface EnvironmentDef {
  id: string;
  label: string;
}

// 'scraped' services are owned by their plugin's discovery and cannot be removed
export type ServiceSource = 'scraped' | 'builtin' | 'manual';

export interface ServiceConfig {
  id: string;
  name: string;
  environment: Environment;
  // The plugin that owns this service's transport and log locations
  packId: string;
  // Kudu base URL, folder path - whatever the plugin's source takes
  endpoint: string | null;
  // True once the user hand-edited the endpoint, so a refresh keeps their value
  endpointManual: boolean;
  // Id of the plugin log location to read; null until detection ran
  location: string | null;
  source: ServiceSource;
}

// --- Plugins (src-tauri/src/plugin.rs) ---

export type FieldType = 'guid' | 'string' | 'number';

// One extracted field, addressed as '<packId>.<key>'
export interface FieldInfo {
  key: string;
  label: string;
  packId: string;
  type: FieldType;
}

export interface LocationInfo {
  id: string;
  label: string;
  dir: string;
  // False for undated growing files, which are filtered by line timestamps
  dated: boolean;
}

export interface PresetDef {
  name: string;
  query: string;
  isRegex: boolean;
}

export interface ChartPresetDef {
  name: string;
  mode: ChartMode;
  bucket: ChartBucket;
  // 'none' | 'service' | 'file' | 'matchedGroup' | 'custom' | 'field:<key>'
  groupBy: string;
  customRegex: string | null;
  metric: ChartMetric;
  // 'field:<key>' | 'custom'
  value: string | null;
  topN: number | null;
}

export type PackOrigin = 'bundled' | { user: { path: string } };

export interface PackInfo {
  id: string;
  name: string;
  version: string;
  description: string;
  origin: PackOrigin;
  // A user pack of this id replaced the bundled one
  overridesBundled: boolean;
  environments: EnvironmentDef[];
  sourceType: 'kudu' | 'local-folder';
  discoveryType: 'none' | 'static' | 'scrape';
  // Button text for a discovery refresh; null when the plugin cannot discover
  discoveryLabel: string | null;
  locations: LocationInfo[];
  fields: FieldInfo[];
  presets: PresetDef[];
  charts: ChartPresetDef[];
}

// A plugin that would not load, so the UI can say which file and why
export interface PackError {
  source: string;
  message: string;
}

export interface PluginsInfo {
  packs: PackInfo[];
  errors: PackError[];
  disabled: string[];
  pluginsDir: string;
  bundled: string[];
}

export interface SavedQuery {
  name: string;
  query: string;
  isRegex: boolean;
}

export interface AppConfig {
  services: ServiceConfig[];
  // Environments the user created, on top of what the plugins declare
  environments: EnvironmentDef[];
  savedQueries: SavedQuery[];
  // Built-in ids already seeded once; must round-trip through update_config,
  // otherwise a deleted built-in comes back on the next start
  seededBuiltins: string[];
  // Plugins the user switched off; a disabled plugin is not loaded at all
  disabledPacks: string[];
  memoryCache: MemoryCacheConfig;
  search: SearchConfig;
}

export interface SearchConfig {
  // A search stops collecting past this many hits and reports the result as
  // truncated. Hits are stored compactly, roughly 100 bytes each.
  maxHits: number;
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
  state: 'downloading' | 'processing' | 'done' | 'skipped' | 'failed';
}

export interface SyncSummary {
  serviceId: string;
  filesTotal: number;
  filesDownloaded: number;
  filesSkipped: number;
  // Files whose download failed; each has a matching entry in warnings
  filesFailed: number;
  bytesDownloaded: number;
  warnings: string[];
}

// Result of downloading one service to a chosen folder: the sync counts plus
// how many cached files were decompressed into the folder
export interface DownloadSummary {
  serviceId: string;
  filesDownloaded: number;
  filesSkipped: number;
  filesFailed: number;
  bytesDownloaded: number;
  filesExported: number;
  bytesExported: number;
  targetDir: string;
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
  // Whole days with nothing cached inside the oldest-newest span, bounded by
  // the exact cached timestamps on either side
  gaps: CoverageGap[];
}

export interface CoverageGap {
  from: string;
  to: string;
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

// A hit's extracted values, keyed by the column key ('<packId>.<field>') the
// plugin declared. Only values actually found are present; SearchMeta.fields
// says what the columns are and how to label them.
export type ExtractedFields = Record<string, string>;

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
  // The columns this result carries, in order. Which fields exist depends on the
  // loaded plugins, so the table takes its extra columns from here.
  fields: FieldInfo[];
  // The configured hit cap cut the result short
  truncated: boolean;
}

// Export or copy the current result, one line per hit. With extract, only the
// matched part (first capture group) of each line; path writes a file (all
// matches), otherwise the capped text comes back for the clipboard.
export interface ExportRequest {
  query: string;
  isRegex: boolean;
  caseSensitive: boolean;
  extract: boolean;
  path: string | null;
  maxLines: number | null;
  // Named groups toggled off in the legend; left out of the extracted output
  excludeGroups: string[];
}

export interface ExportResult {
  text: string | null;
  savedPath: string | null;
  exported: number;
  total: number;
  truncated: boolean;
}

// --- Charts (aggregation of the last search, see src-tauri/src/chart.rs) ---

export type ChartMode = 'timeline' | 'category';
// 'field' groups by one of the loaded plugins' fields, named in groupField
export type ChartGroupBy = 'none' | 'service' | 'file' | 'custom' | 'matchedGroup' | 'field';
export type ChartMetric = 'count' | 'sum' | 'avg' | 'min' | 'max' | 'p50' | 'p95' | 'p99';
export type ChartValueSource = 'field' | 'custom';
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
  // Column key required by groupBy 'field'
  groupField: string | null;
  // Column key required by valueSource 'field'
  valueField: string | null;
  topN: number;
}

export interface ChartSeries {
  name: string;
  // null is a bucket with no data - a gap in a line, no bar in a bar chart
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
