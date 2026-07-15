# LogLooker

Desktop tool for fetching, caching, and cross-searching App Service logs from Kudu.
Tauri 2 + Rust backend, Vue 3 frontend. See PLAN.md for design decisions and the
log source profile catalog.

## Prerequisites

- Azure CLI logged in (`az login`) with access to the Example app services. Auth uses
  `az account get-access-token` bearer tokens against `*.scm.azurewebsites.net` —
  no cookies or stored secrets, all Kudu access is read-only GET.

## Usage

1. **Services** — pick TEST or PRODUCTION, "Refresh from status.example.com" to populate
   the service list (Kudu URLs for the test section are derived from production
   naming). Select services, pick a date range, Sync. Files are cached
   zstd-compressed under `%APPDATA%\LogLooker\cache\<env>\<service>\`;
   unchanged files are skipped, growing files are fetched incrementally (HTTP Range).
2. **Search** — substring or full regex across all selected services, merged into
   one timeline. Click a hit to expand context lines. Save queries or use the
   built-in presets. MediatR fields (`ID_Login`, `ID Command`, operation, duration)
   are extracted per hit.
   - **Chart** tab — aggregates the whole result (not just the loaded page) into a
     bar or line chart: hits over time bucketed by minute/hour/day, split by
     service, operation, login or file; or a top-N category chart. The metric can
     be a count, or sum/avg/p50/p95/p99/min/max of the extracted duration — or of
     a number you capture yourself with a custom regex, where `(?<key>...)` is the
     category and `(?<value>...)` the number. "Open in window" puts the chart in
     its own window that follows the searches you run in the main one.
3. **Log** — the app's own diagnostic log.

## Development

```
npm run tauri dev      # run the app
npm run build          # MSI installer into dist/ (bumps patch version)
cargo test             # unit tests (src-tauri)
cargo test --test e2e -- --ignored --nocapture   # end-to-end against real test env (needs az login)
```
