# LogLooker

Desktop tool for fetching, caching and cross-searching log files from many
services at once. Tauri 2 + Rust backend, Vue 3 frontend.

Where the logs come from, how they are laid out, how their lines parse and what
is worth searching or charting are not built in: they are described by
**plugins** - one JSON file each, no code. See [docs/PLUGINS.md](docs/PLUGINS.md).

## What it does

- **Sync** - downloads the log files a date range needs, caches them
  zstd-compressed under `%APPDATA%\LogLooker\cache\<env>\<service>\`, skips
  unchanged files and fetches only the new tail of growing ones.
- **Search** - substring or full regex across every selected service, merged into
  one timeline. Millions of hits are held compactly and paged on demand. Context
  lines, saved queries, plugin-provided presets.
- **Chart** - aggregates the whole result (not just the loaded page) into a
  timeline or a ranked category chart: count, or sum/avg/p50/p95/p99/min/max of
  any numeric field a plugin extracts - or of a number captured by your own
  regex, where `(?<key>...)` is the category and `(?<value>...)` the number.
  "Open in window" puts the chart in its own window that follows the searches you
  run in the main one.
- **Files** - browse, view and export what is cached, per service.

## Install

```
winget install PsyChonek.LogLooker
```

Or take the `.msi` from
[Releases](https://github.com/PsyChonek/LogLooker/releases). The installer is not
code-signed, so Windows SmartScreen warns on first run ("More info" -> "Run
anyway").

## Getting started

Two plugins ship with the app:

- **Local folder** - reads a directory on this machine or a share. No
  credentials, nothing to set up.
- **Azure App Service (Kudu)** - reads an App Service over the Kudu VFS API,
  authenticating with the Azure CLI. Needs `az login`; all access is read-only
  GETs, and no cookies or secrets are stored.

Then:

1. **Services** - "Add service", pick a plugin, point it at a folder or a Kudu
   URL. A plugin that can discover services gets its own refresh button instead.
2. Select services, pick a date range, **Sync**.
3. **Search** - query them all at once.
4. **Plugins** - what is loaded, what failed to load and why, and the folder to
   drop new packs into.

## Writing a plugin

One JSON file in `%APPDATA%\LogLooker\plugins\`, declaring environments, a
transport, how services are discovered, where the logs live, the timestamp
formats, the fields to extract, and any search or chart presets:

```json
{
  "id": "my-app",
  "name": "My app",
  "source": { "type": "local-folder" },
  "logLocations": [
    {
      "id": "daily",
      "label": "Daily files",
      "dir": ".",
      "file": "^app-(?<y>\\d{4})-(?<m>\\d{2})-(?<d>\\d{2})\\.log$"
    }
  ],
  "timestamps": ["iso8601"],
  "fields": [
    {
      "key": "durationMs",
      "label": "Duration (ms)",
      "gate": "ms",
      "type": "number",
      "regex": " (?<v>\\d+(?:\\.\\d+)?)ms\\b"
    }
  ],
  "presets": [{ "name": "Errors", "query": "ERROR|Exception", "isRegex": true }]
}
```

Fields declared this way become result columns, sort keys and chart dimensions.
Nothing in a plugin executes - every extension point compiles to a regex or an
enum when it loads, which is what keeps third-party plugins safe to install and
keeps the per-line scan running at native speed.

Full reference, including scrape discovery and the validation rules:
[docs/PLUGINS.md](docs/PLUGINS.md). A JSON Schema for editor completion:
[docs/pack.schema.json](docs/pack.schema.json).

## Development

```
npm install
npm run tauri dev            # run the app
npm test                     # typecheck + lint + frontend/script + rust tests
npm run test:run              # frontend and release script tests (Node 24)
npm run typecheck            # vue-tsc
npm run lint                 # eslint
npm run test:rust            # cargo test
npm run build:frontend       # typecheck + bundle
```

End-to-end against a real source is opt-in and ignored by default:

```
$env:LOGLOOKER_E2E_FOLDER = "C:\some\logs"
cd src-tauri && cargo test --test e2e -- --ignored --nocapture
```

`npm run build` produces the MSI installer into `dist/` and bumps the patch
version. It is for trying a real build locally - **releases go through CI**, which
does its own bump, so a local build followed by a CI release skips a version
number.

## CI/CD

| Workflow | Trigger | Does |
| --- | --- | --- |
| **CI** | push/PR on `main`, nightly | Typecheck, lint, frontend/script tests and bundle; Rust tests |
| **Release** | manual (`workflow_dispatch`) on `main` | Run CI, bump the version, build x64 and ARM64 MSI/NSIS installers, regenerate winget manifests, commit, tag, publish a GitHub release, open a winget PR |

Both workflows use **GitHub-hosted Windows runners** (`windows-latest`) and Node 24,
following the same manual release flow as SqlPlanForDummies.

To release: **Actions -> Release -> Run workflow**, pick `patch`/`minor`/`major`.
It synchronizes `package.json`, `package-lock.json`, `tauri.conf.json`,
`Cargo.toml` and `Cargo.lock`, commits the bump and regenerated `winget/` manifests
to `main`, tags `v<version>`, and attaches both architectures' MSI/NSIS installers
and `SHA256SUMS.txt`. Releases are serialized and existing tags are never replaced.
Use `skip_tests` only when deliberately bypassing CI, and `skip_winget` to omit
the winget submission. Winget submission is automatically skipped while the
repository is private, since its installer URLs must be publicly accessible.

Two things it needs:

- **`WINGET_TOKEN`** secret in the `release` environment - a PAT with `public_repo`
  scope, and a fork of
  [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) under your
  account. Without it, run with `skip_winget` and submit by hand (see
  [winget/README.md](winget/README.md)).
- **Push access to `main`** for `github-actions[bot]`, since it commits the bump.
  Branch protection has to allow it.

The version update can be previewed without writing files or publishing:

```
node scripts/bump-version.mjs patch --dry-run
```

The manifests can also be produced and submitted from a local build:

```powershell
npm run build
./scripts/update-winget-manifests.ps1
./scripts/submit-to-winget.ps1
```

## License

MIT - see [LICENSE](LICENSE).
