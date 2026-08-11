# Architecture

Tauri 2 + Vue 3. The Rust side does the fetching, caching, scanning and
aggregating; the frontend is a view over it. Nothing about a particular log
format lives in either - that comes from plugins (see [PLUGINS.md](PLUGINS.md)).

## Rust (`src-tauri/src/`)

| Module | Responsibility |
| --- | --- |
| `plugin.rs` | Pack manifests: parse, validate, compile to regexes. `Registry` is the loaded set, held behind an `Arc` so a search can take a snapshot and scan without a lock. |
| `source.rs` | Transports, as a plain enum (`Kudu`, `LocalFolder`). List a directory, copy a file, optionally only its tail. |
| `kudu.rs` | The Kudu VFS client: streamed downloads, `Range` for growing files, retries that resume rather than restart, a stall timeout, and a bail-out when a server ignores `Range`. |
| `auth.rs` | Azure token from `az account get-access-token`, with an expiry cache. |
| `discovery.rs` | Declarative service discovery: a pack's static list, or a page scraped by its own regexes. |
| `locations.rs` | Probes a pack's log locations and lists the files a date range needs. |
| `cache.rs` | zstd store under `<data_dir>/LogLooker/cache/<env>/<service>/`, plus a manifest that lets an unchanged file be skipped. Also detects each service's log clock. |
| `growing.rs` | Undated growing files: their lines are split into per-day segments that survive the remote file rotating under the read. |
| `memcache.rs` | Optional RAM tier holding decompressed files between searches, under a budget. |
| `search.rs` | The scan: rayon over files, and over line-aligned chunks within a file. `Matcher` picks the fast regex engine or the backtracking one. |
| `fields.rs` | Extracted values, one column per field, text interned to a `u32`. |
| `chart.rs` | Aggregation of a whole result into timeline or category buckets. |
| `rawfile.rs` | The single-file viewer and its in-file search. |
| `config.rs` | `config.json` under `<config_dir>/LogLooker/`, and the migration that binds pre-plugin services to a pack. |
| `commands/` | Tauri commands, all `Result<T, String>`. |

## Decisions worth knowing

**Plugins are data, not code.** Every extension point compiles to a regex or an
enum at load. That is what makes a third-party pack safe to install, and it is
also the only way the per-line hot path stays at native speed - a plugin boundary
crossed per line would not.

**Hits are compact and text-free.** A search can match millions of lines, so a
stored hit carries no line text: a file index, a line number, a timestamp, a
group mask and a row index. Line text and context are re-read from the cached
file when a page is displayed, so neither RAM nor IPC ever holds it all.

**Fields are columns, not per-hit structs.** Values live in one column per field -
text interned to a `u32`, numbers as `f64`. A hit references its values by row
index rather than by position, so re-sorting a result reorders the hits and leaves
the columns alone. Parallel scan units intern independently and their dictionaries
are remapped when their columns are concatenated.

**An entry is one hit.** A timestamped line opens an entry; the continuation lines
that follow (stack traces, SQL bodies, embedded HTML) belong to it and inherit its
timestamp. However many of its lines match, the entry is one hit anchored at its
header line. Chunk boundaries and file rotation both split entries, so a chunk's
leading matches are held as an "orphan" and stitched to the header they belong to.

**Never attribute a field by proximity.** Concurrent services interleave their
output, so a value only belongs to an entry if it was read off that entry's own
lines.

**The log clock is measured, not assumed.** A log line carries no zone. A host
writes UTC unless configured otherwise, and a file's last write is its last
timestamped line, so the offset is `median(last_ts - remote_mtime)` over a
service's cached files, snapped to a quarter hour. That works on files of any age,
unlike comparing the newest line to "now", which only speaks while a service is
actively logging. Times display on a UTC/local toggle, each labelled.

**Downloads are capped at the listed size.** A file being appended to while it is
read would otherwise keep the transfer open indefinitely; bytes past the size the
listing promised are left for the next sync.

**A partial download is a valid prefix.** Whatever arrived stays on disk, so the
next ranged sync resumes past it instead of starting over.

## Frontend (`src/`)

Views: Services, Search (with the chart tab), Files, Plugins, Log. Pinia store in
`stores/appStore.ts` holds the config, the loaded plugins and the selection.

Everything the UI offers as a choice - environments, log locations, result
columns, sort keys, chart dimensions - is derived from the loaded packs. The
result's own column list travels with its metadata (`SearchMeta.fields`), so the
table shows the columns of the search that produced it.

Chart and raw-file windows are separate webviews. Two constraints they must
respect, both learned the hard way:

- A runtime window must pass exactly the same `additionalBrowserArgs` as the main
  window in `tauri.conf.json`. WebView2 refuses to create a second environment
  with different options in one user-data folder, and the window never opens.
- Every window must disable Tauri's drag-drop handler, or HTML5 drag (column
  reordering) silently stops working.
