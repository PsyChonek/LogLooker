# Writing a LogLooker plugin

A plugin (a "pack") is one JSON file describing a family of log sources: where
services come from, where each one keeps its logs, how its lines parse, and what
is worth searching and charting.

Nothing in a pack executes. Every extension point is data that compiles to a
regex or an enum when the pack loads, which is what keeps a pack from the internet
safe to install and keeps the per-line scan running at native speed.

## Where packs live

| Location | Purpose |
| --- | --- |
| `%APPDATA%\LogLooker\plugins\*.json` | Your packs. The **Plugins** tab has a button that opens this folder. |
| bundled | `azure-kudu` and `local-folder` ship with the app. |

Bundled packs load first, then user packs. **A user pack with the id of a bundled
one replaces it**, so a shipped pack can be corrected locally without waiting for
a release.

Several packs are active at once and their capabilities union. Field keys are
therefore namespaced as `<packId>.<fieldKey>`: two packs may both extract
something called `operation` without one standing in for the other. Environment
ids are *not* namespaced - one TEST toggle is meant to show every pack's test
services together.

The **Plugins** tab lists what loaded, and lists what did not with the file name
and the reason. A pack with a typo is a broken pack, never a missing one.
"Reload plugins" re-reads the folder, so editing a pack does not need a restart.

## Skeleton

```json
{
  "$schema": "https://raw.githubusercontent.com/PsyChonek/LogLooker/main/docs/pack.schema.json",
  "id": "my-app",
  "name": "My app",
  "version": "1.0.0",
  "description": "What this pack reads.",

  "environments": [{ "id": "test", "label": "TEST" }],
  "source": { "type": "local-folder" },
  "discovery": { "type": "none" },
  "logLocations": [],
  "timestamps": ["iso8601"],
  "fields": [],
  "presets": [],
  "charts": []
}
```

`id` and `name` are required; everything else has a default. Unknown keys are an
error rather than being ignored, so a typo surfaces instead of silently doing
nothing.

## `id`

Lowercase letters, digits and hyphens, starting with a letter or digit. It
namespaces the pack's field keys and is what a service records to say which pack
owns it, so changing it orphans existing services.

## `environments`

```json
"environments": [
  { "id": "test", "label": "TEST" },
  { "id": "production", "label": "PRODUCTION" }
]
```

An environment id is also a cache directory name, so it follows the same rules as
a pack id. Omitting the list gives one environment, `default`.

What a pack declares here is what it contributes to the app-wide environment
list, and what its discovery may assign services to. It is not a restriction: the
user can add environments of their own in the Services tab, and a service of any
pack can be put in any environment.

## `source`

The transport. The list is closed - a pack picks one and configures it.

```json
"source": { "type": "kudu", "auth": "az-cli" }
```

Reads an Azure App Service over the Kudu VFS API. A service's endpoint is its
`https://<app>.scm.azurewebsites.net` URL. `auth` only accepts `az-cli`: a bearer
token from `az account get-access-token`, which needs `az login`. Plain http is
refused, because the token travels in a header.

```json
"source": { "type": "local-folder", "root": "C:\\logs" }
```

Reads a directory on this machine or a mounted share. A service's endpoint is its
folder; a relative one hangs off `root`, an absolute one stands alone so a root
cannot silently redirect it. `root` is optional.

## `discovery`

How the service list gets filled in. Discovery is always optional and never
authoritative - it merges into an editable list, so a status page changing its
markup degrades to "the refresh button stops finding anything".

### `none` (default)

Services are added by hand.

### `static`

Hosts no page knows about, shipped with the pack. Seeded once and deletable
afterwards: deleting one sticks rather than it reappearing on the next start.

```json
"discovery": {
  "type": "static",
  "services": [
    {
      "name": "WebJobs",
      "environment": "test",
      "endpoint": "https://my-test-webjobs.scm.azurewebsites.net",
      "location": "core-applogs"
    }
  ]
}
```

`location` is optional and names one of the pack's `logLocations`.

### `scrape`

Parsed out of an HTML page. One `container` match per service; each service's own
regexes then run over the block from its container match to the next one, which is
what keeps a link from being attributed to the service above it.

```json
"discovery": {
  "type": "scrape",
  "url": "https://status.example.com/",
  "label": "Refresh from status.example.com",
  "container": "data-section-name=\"(?<section>[^\"]*)\"\\s+data-page-name=\"(?<name>[^\"]*)\"",
  "endpoint": "href=\"(https://[^\"]*\\.scm\\.azurewebsites\\.net)[/\"]",
  "logDir": "href=\"https://[^\"]*/filemanager/([^\"]*)\"",
  "environments": [{ "contains": "prod", "environment": "production" }],
  "defaultEnvironment": "test"
}
```

| Key | Meaning |
| --- | --- |
| `url` | The page to fetch. |
| `label` | Button text. Defaults to `Refresh from <url>`. |
| `container` | Run over the whole page. Needs a `name` group; a `section` group is what environment rules match on. |
| `endpoint` | Run over one service's block. Group `endpoint`, or the first plain group, is the URL or path. |
| `logDir` | Run over one service's block. Group `dir`, or the first plain group, is matched against the `dir` of each log location, saving a probe. |
| `environments` | First rule whose `contains` appears in the `section` text wins, case-insensitively. |
| `defaultEnvironment` | For services no rule matched. Defaults to the first declared environment. |

A service the page lists without a link keeps no endpoint; you can fill it in by
hand afterwards. An unrecognised `logDir` is simply not a hint - detection still
probes.

## `logLocations`

Where a service keeps its logs. The app probes them **in declaration order** and
takes the first that holds a matching file, so put the most specific first and any
catch-all last. The choice is stored per service and stays overridable in the
Services table.

```json
"logLocations": [
  {
    "id": "core-applogs",
    "label": "NLog applogs (one file per day)",
    "dir": "applogs",
    "file": "^log-(?<y>\\d{4})-(?<m>\\d{2})-(?<d>\\d{2})\\.log$"
  },
  {
    "id": "app-data",
    "label": "App_Data/Log.txt",
    "dir": "site/wwwroot/App_Data",
    "file": "(?i)^Log\\.txt$",
    "dated": false
  }
]
```

| Key | Meaning |
| --- | --- |
| `id` | Lowercase slug, unique in the pack. Stored in config, so renaming it re-detects. |
| `label` | Shown in the Services table. |
| `dir` | Directory relative to the service's endpoint. `.` is the endpoint itself. |
| `file` | File name regex. Prefix `(?i)` for a case-insensitive name - Windows hosts are case-insensitive about theirs. An optional `instance` group marks a scale-out instance id, and several instances in one range raise a coverage warning. |
| `dated` | Default `true`: the name carries the day it covers, `file` needs `y`/`m`/`d` groups, and a date range picks files by name. `false`: the files are undated and growing, everything matching is fetched, and only the line timestamps can narrow the range. |

An undated location is synced differently: its lines are split into per-day
segments that survive the remote file being rotated under the read.

## `timestamps`

The line-start formats to try, in order. Omitting the list tries every built-in.

| Value | Matches |
| --- | --- |
| `"iso8601"` | `2026-07-15T08:08:09.9533870Z`, also with a space instead of the `T`. Any fractional width; a trailing `Z` or offset is ignored. |
| `"dotted-dmy"` | `13.07.2026 00:00:02.611`, fractional part optional. |
| `"us-mdy"` | `7/14/2026 1:15:55 AM`. |
| `{ "regex": "..." }` | Your own, from named groups `y`, `m`, `d`, `H`, `M`, `S`, and optionally `f` for fractional seconds. |

The named formats are hand-rolled byte parsers. **A `regex` format runs a regex on
every line of every file and is measurably slower** - it is the slow path you opt
into knowingly. Lines that match nothing (stack traces, SQL bodies, embedded HTML)
inherit the timestamp of the entry they belong to, which is how a multi-line entry
stays one hit.

## `fields`

The values pulled out of a matching line. Each becomes a result column, a sort key
and a chart dimension.

```json
"fields": [
  {
    "key": "durationMs",
    "label": "Duration (ms)",
    "gate": "ms",
    "type": "number",
    "regex": " (?<v>\\d+(?:\\.\\d+)?)ms\\b"
  }
]
```

| Key | Meaning |
| --- | --- |
| `key` | Letters, digits and underscores, starting with a letter. Addressed as `<packId>.<key>`. |
| `label` | Column header. |
| `gate` | Optional literal the line must contain before the regex runs. |
| `regex` | The value comes from group `v`, or from the first plain group. |
| `type` | `string` (default), `number`, or `guid`. |

**Use `gate`.** Extraction runs on every hit, and an unfiltered query makes a hit
of every line in the range; a substring test costs a fraction of a capture run.

`number` values that will not parse, or are not finite, are dropped rather than
stored - only a `number` field can be aggregated by a chart metric. `guid` values
that are not hyphenated GUIDs are dropped, and the rest are lowercased so the same
id written in two cases is one category rather than two.

At most 32 fields per pack. Values are interned per column, so a field whose
values repeat costs almost nothing per hit; a field with a distinct value on every
line is the expensive kind.

Values are read only off the entry's own lines. Never expect a field to be filled
in from a neighbouring line: concurrent services interleave their output, so
proximity means nothing.

## `presets`

Search queries the pack offers alongside the user's saved ones.

```json
"presets": [
  { "name": "Errors", "query": "\\b(ERROR|FATAL)\\b", "isRegex": true },
  { "name": "Exceptions", "query": "Exception", "isRegex": false }
]
```

With more than one pack loaded, preset names are prefixed with the pack name so
identically named ones stay tellable apart.

## `charts`

Ready-made charts.

```json
"charts": [
  { "name": "Hits over time", "mode": "timeline", "bucket": "auto", "groupBy": "none" },
  {
    "name": "Slowest requests",
    "mode": "category",
    "groupBy": "field:request",
    "metric": "p95",
    "value": "field:durationMs",
    "topN": 20
  }
]
```

| Key | Values |
| --- | --- |
| `mode` | `timeline` (x axis is time) or `category` (x axis is the group value, ranked and cut to `topN`). |
| `bucket` | `auto`, `second`, `minute`, `fiveMinutes`, `fifteenMinutes`, `hour`, `sixHours`, `day`. |
| `groupBy` | `none`, `service`, `file`, `matchedGroup` (one series per named group of the query), `custom`, or `field:<key>` naming one of this pack's fields. |
| `metric` | `count`, `sum`, `avg`, `min`, `max`, `p50`, `p95`, `p99`. |
| `value` | Where a non-count metric's number comes from: `field:<key>` (must be a `number` field) or `custom`. |
| `customRegex` | Required by `groupBy: custom` or `value: custom`. The category is the `key` group, the number the `value` group. |
| `topN` | Series (timeline) or bars (category) to keep. |

Field references use the **local** key, without the pack prefix - a pack refers to
its own fields.

## Validation

A pack is validated when it loads, and the **Plugins** tab shows the file name and
the message for anything that failed. The checks worth knowing about:

- `id`, environment ids and location ids must be lowercase path-safe slugs.
- Unknown keys anywhere are an error (a typo, not a silent no-op).
- A field regex must have a `v` group or at least one plain group.
- A `dated` location's `file` must have `y`, `m` and `d` groups.
- A custom timestamp regex must have `y`, `m`, `d`, `H`, `M`, `S`.
- A chart cannot group or measure by a field the pack does not declare, and cannot
  aggregate a field that is not a `number`.
- A non-count metric needs a `value`.
- Scrape `container` needs a `name` group, and environment rules need a `section`
  group to match against.

## Keeping a pack private

A pack in `%APPDATA%\LogLooker\plugins\` is yours and is never part of this repo.
If your pack carries internal host names, an internal status page URL or a service
inventory, keep it there rather than contributing it - the bundled packs are
deliberately generic for that reason.
