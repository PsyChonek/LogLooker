//! Aggregation of a finished search into chart-ready buckets.
//!
//! Runs over the full hit list held in `SearchState`, not over the pages the UI
//! has loaded, so a chart always describes the whole result. Two shapes:
//!
//! - `Timeline` — x axis is a time bucket, one series per group (service,
//!   operation, ...). "Errors per hour, split by service."
//! - `Category` — x axis is the group value itself, one bar each, ranked and
//!   cut to `top_n`. "Top 10 operations by p95 duration."

use crate::search::SearchHit;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Auto bucketing aims for at most this many points; explicit bucket choices
/// may produce more, up to MAX_BUCKETS.
const TARGET_BUCKETS: i64 = 200;
/// Hard cap — beyond this the SVG is unreadable and the payload pointless.
const MAX_BUCKETS: usize = 2000;
const MAX_SERIES: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChartMode {
    Timeline,
    Category,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GroupBy {
    None,
    Service,
    Operation,
    IdLogin,
    File,
    Custom,
    /// One series per named capture group of the search query — a hit that
    /// matched several groups counts in each of them
    MatchedGroup,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Metric {
    Count,
    Sum,
    Avg,
    Min,
    Max,
    P50,
    P95,
    P99,
}

impl Metric {
    fn needs_value(self) -> bool {
        self != Metric::Count
    }

    fn keeps_values(self) -> bool {
        matches!(self, Metric::P50 | Metric::P95 | Metric::P99)
    }
}

/// Where the number behind a non-count metric comes from.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ValueSource {
    /// The `duration_ms` extracted from MediatR timing lines
    Duration,
    /// The `value` capture group of the custom regex
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Bucket {
    Auto,
    Second,
    Minute,
    FiveMinutes,
    FifteenMinutes,
    Hour,
    SixHours,
    Day,
}

impl Bucket {
    fn seconds(self) -> i64 {
        match self {
            Bucket::Auto => 0,
            Bucket::Second => 1,
            Bucket::Minute => 60,
            Bucket::FiveMinutes => 300,
            Bucket::FifteenMinutes => 900,
            Bucket::Hour => 3600,
            Bucket::SixHours => 21600,
            Bucket::Day => 86400,
        }
    }

    fn label(seconds: i64) -> String {
        match seconds {
            1 => "1 s".into(),
            60 => "1 min".into(),
            300 => "5 min".into(),
            900 => "15 min".into(),
            3600 => "1 hour".into(),
            21600 => "6 hours".into(),
            86400 => "1 day".into(),
            other => format!("{other} s"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartRequest {
    pub mode: ChartMode,
    pub bucket: Bucket,
    pub group_by: GroupBy,
    /// Required by `GroupBy::Custom` and `ValueSource::Custom`. The category
    /// comes from the `key` named group (or group 1), the number from the
    /// `value` named group (or group 2).
    pub custom_regex: Option<String>,
    pub metric: Metric,
    pub value_source: ValueSource,
    pub top_n: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartSeries {
    pub name: String,
    /// One entry per label. `None` is "no data in this bucket" — a gap in a
    /// line chart, no bar in a bar chart. Counts never produce gaps.
    pub values: Vec<Option<f64>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartData {
    /// Timeline: bucket start as `2026-07-14T13:05:00`. Category: the group value.
    pub labels: Vec<String>,
    pub series: Vec<ChartSeries>,
    pub bucket_seconds: i64,
    pub bucket_label: String,
    pub metric_label: String,
    /// Hits that made it into the chart
    pub charted: usize,
    /// Hits dropped for want of a timestamp, a value, or a key match
    pub skipped_no_time: usize,
    pub skipped_no_value: usize,
    pub skipped_no_key: usize,
    /// Groups that exist but did not make the top_n cut
    pub other_groups: usize,
    /// Minutes the charted labels run ahead of UTC. Buckets are cut from the log
    /// lines' own clock, so this is only known when every charted service shares
    /// one — the aggregate itself cannot tell, so the command fills it in.
    pub log_offset_minutes: Option<i32>,
}

/// Streams the aggregate of one (series, bucket) cell. Raw values are only
/// retained for percentiles — a count over millions of hits stays O(1) memory.
#[derive(Default)]
struct Cell {
    count: u64,
    sum: f64,
    min: Option<f64>,
    max: Option<f64>,
    values: Vec<f64>,
}

impl Cell {
    fn add(&mut self, value: Option<f64>, keep_values: bool) {
        self.count += 1;
        if let Some(v) = value {
            self.sum += v;
            self.min = Some(self.min.map_or(v, |m| m.min(v)));
            self.max = Some(self.max.map_or(v, |m| m.max(v)));
            if keep_values {
                self.values.push(v);
            }
        }
    }

    fn value(&self, metric: Metric) -> Option<f64> {
        match metric {
            Metric::Count => Some(self.count as f64),
            Metric::Sum => Some(self.sum),
            Metric::Avg if self.count > 0 => Some(self.sum / self.count as f64),
            Metric::Avg => None,
            Metric::Min => self.min,
            Metric::Max => self.max,
            Metric::P50 => percentile(&self.values, 0.50),
            Metric::P95 => percentile(&self.values, 0.95),
            Metric::P99 => percentile(&self.values, 0.99),
        }
    }
}

/// Nearest-rank percentile over an unsorted sample.
fn percentile(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = (p * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted.get(rank - 1).copied()
}

fn compile_custom(request: &ChartRequest) -> Result<Option<Regex>, String> {
    let needed = request.group_by == GroupBy::Custom
        || (request.metric.needs_value() && request.value_source == ValueSource::Custom);
    if !needed {
        return Ok(None);
    }
    let pattern = request
        .custom_regex
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .ok_or("Custom grouping needs a regex")?;
    let regex = Regex::new(pattern).map_err(|e| format!("Invalid regex: {e}"))?;
    if request.group_by == GroupBy::Custom
        && regex.capture_names().flatten().all(|n| n != "key")
        && regex.captures_len() < 2
    {
        return Err("The regex needs a capture group for the category — name it (?<key>...) or use the first group".into());
    }
    if request.metric.needs_value()
        && request.value_source == ValueSource::Custom
        && regex.capture_names().flatten().all(|n| n != "value")
        && regex.captures_len() < 3
    {
        return Err("The regex needs a capture group for the number — name it (?<value>...) or use the second group".into());
    }
    Ok(Some(regex))
}

/// Pulls the category and the number out of one line in a single regex pass.
fn custom_capture(regex: &Regex, line: &str) -> Option<(Option<String>, Option<f64>)> {
    let caps = regex.captures(line)?;
    let key = caps
        .name("key")
        .or_else(|| caps.get(1))
        .map(|m| m.as_str().to_string());
    let value = caps
        .name("value")
        .or_else(|| caps.get(2))
        .and_then(|m| m.as_str().trim().replace(',', ".").parse::<f64>().ok());
    Some((key, value))
}

/// Smallest bucket that keeps the span under TARGET_BUCKETS points.
fn auto_bucket(span_seconds: i64) -> i64 {
    const LADDER: [i64; 7] = [1, 60, 300, 900, 3600, 21600, 86400];
    for size in LADDER {
        if span_seconds / size <= TARGET_BUCKETS {
            return size;
        }
    }
    86400
}

fn metric_label(request: &ChartRequest) -> String {
    let source = match request.value_source {
        ValueSource::Duration => "duration (ms)",
        ValueSource::Custom => "custom value",
    };
    match request.metric {
        Metric::Count => "Hits".into(),
        Metric::Sum => format!("Total {source}"),
        Metric::Avg => format!("Avg {source}"),
        Metric::Min => format!("Min {source}"),
        Metric::Max => format!("Max {source}"),
        Metric::P50 => format!("p50 {source}"),
        Metric::P95 => format!("p95 {source}"),
        Metric::P99 => format!("p99 {source}"),
    }
}

pub fn aggregate(
    hits: &[SearchHit],
    request: &ChartRequest,
    service_names: &HashMap<String, String>,
    group_names: &[String],
) -> Result<ChartData, String> {
    let custom = compile_custom(request)?;
    let keep_values = request.metric.keeps_values();
    let top_n = request.top_n.clamp(1, MAX_SERIES);

    if request.group_by == GroupBy::MatchedGroup && group_names.is_empty() {
        return Err(
            "The search query has no capture groups — build one with the query builder or write (?<name>...) groups"
                .into(),
        );
    }

    let mut skipped_no_time = 0;
    let mut skipped_no_value = 0;
    let mut skipped_no_key = 0;
    let mut charted = 0;

    // Key -> bucket start (timeline) or Key -> single cell (category)
    let mut cells: HashMap<String, HashMap<i64, Cell>> = HashMap::new();
    let mut group_counts: HashMap<String, u64> = HashMap::new();
    let mut min_ts = i64::MAX;
    let mut max_ts = i64::MIN;

    // Pass 1 — extract (key, value, timestamp) per hit and note the time span.
    // Bucketing needs the span first when it is Auto, so keep the extracted
    // triples instead of re-parsing lines in pass 2.
    let mut points: Vec<(String, Option<f64>, i64)> = Vec::new();

    for hit in hits {
        let (custom_key, custom_value) = match &custom {
            Some(regex) => custom_capture(regex, &hit.line).unwrap_or((None, None)),
            None => (None, None),
        };

        // Most groupings key a hit once; a hit that matched several capture
        // groups belongs to each of their series.
        let keys: Vec<String> = match request.group_by {
            GroupBy::None => vec!["All hits".to_string()],
            GroupBy::Service => vec![service_names
                .get(&hit.service_id)
                .cloned()
                .unwrap_or_else(|| hit.service_id.clone())],
            GroupBy::Operation => hit.fields.operation.clone().into_iter().collect(),
            GroupBy::IdLogin => hit.fields.id_login.clone().into_iter().collect(),
            GroupBy::File => vec![hit.file.clone()],
            GroupBy::Custom => custom_key.into_iter().collect(),
            GroupBy::MatchedGroup => hit.matched_groups.clone(),
        };
        if keys.is_empty() {
            skipped_no_key += 1;
            continue;
        }

        let value = if request.metric.needs_value() {
            let value = match request.value_source {
                ValueSource::Duration => hit.fields.duration_ms,
                ValueSource::Custom => custom_value,
            };
            if value.is_none() {
                skipped_no_value += 1;
                continue;
            }
            value
        } else {
            None
        };

        let epoch = match request.mode {
            ChartMode::Timeline => match hit.timestamp {
                Some(ts) => ts.and_utc().timestamp(),
                None => {
                    skipped_no_time += 1;
                    continue;
                }
            },
            ChartMode::Category => 0,
        };

        min_ts = min_ts.min(epoch);
        max_ts = max_ts.max(epoch);
        charted += 1;
        for key in keys {
            *group_counts.entry(key.clone()).or_default() += 1;
            points.push((key, value, epoch));
        }
    }

    if points.is_empty() {
        return Ok(ChartData {
            labels: Vec::new(),
            series: Vec::new(),
            bucket_seconds: 0,
            bucket_label: String::new(),
            metric_label: metric_label(request),
            charted: 0,
            skipped_no_time,
            skipped_no_value,
            skipped_no_key,
            other_groups: 0,
            log_offset_minutes: None,
        });
    }

    let bucket_seconds = match (request.mode, request.bucket) {
        (ChartMode::Category, _) => 0,
        (ChartMode::Timeline, Bucket::Auto) => auto_bucket(max_ts - min_ts),
        (ChartMode::Timeline, explicit) => explicit.seconds(),
    };

    if request.mode == ChartMode::Timeline {
        let count = ((max_ts - min_ts) / bucket_seconds) as usize + 1;
        if count > MAX_BUCKETS {
            return Err(format!(
                "{count} buckets of {} would be unreadable — pick a coarser bucket",
                Bucket::label(bucket_seconds)
            ));
        }
    }

    // Rank groups. Timeline series are ranked by hit count (a series that is
    // rare overall does not deserve a colour); category bars by the metric
    // itself, so "top 10 by p95" means what it says. Matched groups instead
    // keep pattern order — their colour must match the query editor and the
    // highlighted hits, and a group that never matched still shows as flat.
    let mut ranked: Vec<String> = if request.group_by == GroupBy::MatchedGroup {
        group_names.to_vec()
    } else {
        group_counts.keys().cloned().collect()
    };

    // Pass 2 — fill the cells.
    for (key, value, epoch) in &points {
        let bucket = if bucket_seconds > 0 {
            epoch - epoch.rem_euclid(bucket_seconds)
        } else {
            0
        };
        cells
            .entry(key.clone())
            .or_default()
            .entry(bucket)
            .or_default()
            .add(*value, keep_values);
    }

    if request.group_by != GroupBy::MatchedGroup {
        match request.mode {
            ChartMode::Timeline => {
                ranked.sort_by(|a, b| {
                    group_counts[b]
                        .cmp(&group_counts[a])
                        .then_with(|| a.cmp(b))
                });
            }
            ChartMode::Category => {
                let metric_of = |key: &String| {
                    cells
                        .get(key)
                        .and_then(|b| b.get(&0))
                        .and_then(|cell| cell.value(request.metric))
                        .unwrap_or(f64::NEG_INFINITY)
                };
                ranked.sort_by(|a, b| {
                    metric_of(b)
                        .partial_cmp(&metric_of(a))
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.cmp(b))
                });
            }
        }
    }

    let other_groups = if request.group_by == GroupBy::MatchedGroup {
        // Never cut matched groups to top_n: dropping one would silently
        // reassign the colours of those after it
        ranked.truncate(MAX_SERIES);
        0
    } else {
        let other = ranked.len().saturating_sub(top_n);
        ranked.truncate(top_n);
        other
    };

    let (labels, series) = match request.mode {
        ChartMode::Timeline => {
            let start = min_ts - min_ts.rem_euclid(bucket_seconds);
            let mut bucket_starts = Vec::new();
            let mut at = start;
            while at <= max_ts {
                bucket_starts.push(at);
                at += bucket_seconds;
            }
            let labels = bucket_starts
                .iter()
                .map(|epoch| {
                    chrono::DateTime::from_timestamp(*epoch, 0)
                        .map(|dt| dt.naive_utc().format("%Y-%m-%dT%H:%M:%S").to_string())
                        .unwrap_or_default()
                })
                .collect();
            let series = ranked
                .iter()
                .map(|key| {
                    let buckets = cells.get(key);
                    let values = bucket_starts
                        .iter()
                        .map(|epoch| match buckets.and_then(|b| b.get(epoch)) {
                            Some(cell) => cell.value(request.metric),
                            // A count knows an empty bucket is zero; an average
                            // of nothing is not zero, it is nothing.
                            None if request.metric == Metric::Count => Some(0.0),
                            None => None,
                        })
                        .collect();
                    ChartSeries {
                        name: key.clone(),
                        values,
                    }
                })
                .collect();
            (labels, series)
        }
        ChartMode::Category => {
            let values = ranked
                .iter()
                .map(|key| {
                    cells
                        .get(key)
                        .and_then(|b| b.get(&0))
                        .and_then(|cell| cell.value(request.metric))
                })
                .collect();
            (
                ranked.clone(),
                vec![ChartSeries {
                    name: metric_label(request),
                    values,
                }],
            )
        }
    };

    Ok(ChartData {
        labels,
        series,
        bucket_seconds,
        bucket_label: if bucket_seconds > 0 {
            Bucket::label(bucket_seconds)
        } else {
            String::new()
        },
        metric_label: metric_label(request),
        charted,
        skipped_no_time,
        skipped_no_value,
        skipped_no_key,
        other_groups,
        log_offset_minutes: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::ExtractedFields;
    use chrono::NaiveDateTime;

    fn hit(service: &str, ts: Option<&str>, line: &str, duration: Option<f64>) -> SearchHit {
        SearchHit {
            service_id: service.into(),
            file: "log-2026-07-14.log".into(),
            line_number: 1,
            timestamp: ts.map(|t| NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S").unwrap()),
            line: line.into(),
            context_before: Vec::new(),
            context_after: Vec::new(),
            fields: ExtractedFields {
                duration_ms: duration,
                operation: Some("PostOrder".into()),
                ..Default::default()
            },
            matched_groups: Vec::new(),
        }
    }

    fn request(mode: ChartMode, group_by: GroupBy, metric: Metric) -> ChartRequest {
        ChartRequest {
            mode,
            bucket: Bucket::Hour,
            group_by,
            custom_regex: None,
            metric,
            value_source: ValueSource::Duration,
            top_n: 10,
        }
    }

    #[test]
    fn counts_hits_per_bucket_and_fills_gaps_with_zero() {
        let hits = vec![
            hit("a", Some("2026-07-14T10:05:00"), "boom", None),
            hit("a", Some("2026-07-14T10:59:00"), "boom", None),
            hit("a", Some("2026-07-14T12:00:00"), "boom", None),
        ];
        let data = aggregate(&hits, &request(ChartMode::Timeline, GroupBy::None, Metric::Count), &HashMap::new(), &[]).unwrap();

        assert_eq!(data.labels.len(), 3, "10:00, 11:00 (empty), 12:00");
        assert_eq!(data.series.len(), 1);
        assert_eq!(data.series[0].values, vec![Some(2.0), Some(0.0), Some(1.0)]);
        assert_eq!(data.charted, 3);
    }

    #[test]
    fn splits_series_by_service_using_display_names() {
        let hits = vec![
            hit("test/client-api", Some("2026-07-14T10:05:00"), "boom", None),
            hit("test/internal-api", Some("2026-07-14T10:07:00"), "boom", None),
            hit("test/client-api", Some("2026-07-14T10:09:00"), "boom", None),
        ];
        let names = HashMap::from([
            ("test/client-api".to_string(), "client-api".to_string()),
            ("test/internal-api".to_string(), "internal-api".to_string()),
        ]);
        let data = aggregate(&hits, &request(ChartMode::Timeline, GroupBy::Service, Metric::Count), &names, &[]).unwrap();

        assert_eq!(data.series.len(), 2);
        // Ranked by hit count, so the two-hit service leads
        assert_eq!(data.series[0].name, "client-api");
        assert_eq!(data.series[0].values, vec![Some(2.0)]);
        assert_eq!(data.series[1].name, "internal-api");
        assert_eq!(data.series[1].values, vec![Some(1.0)]);
    }

    #[test]
    fn averages_duration_and_leaves_empty_buckets_as_gaps() {
        let hits = vec![
            hit("a", Some("2026-07-14T10:00:00"), "done", Some(100.0)),
            hit("a", Some("2026-07-14T10:30:00"), "done", Some(300.0)),
            hit("a", Some("2026-07-14T12:00:00"), "done", Some(50.0)),
        ];
        let data = aggregate(&hits, &request(ChartMode::Timeline, GroupBy::None, Metric::Avg), &HashMap::new(), &[]).unwrap();

        assert_eq!(data.series[0].values, vec![Some(200.0), None, Some(50.0)]);
        assert_eq!(data.metric_label, "Avg duration (ms)");
    }

    #[test]
    fn hits_without_a_value_are_skipped_not_zeroed() {
        let hits = vec![
            hit("a", Some("2026-07-14T10:00:00"), "done", Some(100.0)),
            hit("a", Some("2026-07-14T10:10:00"), "no duration here", None),
        ];
        let data = aggregate(&hits, &request(ChartMode::Timeline, GroupBy::None, Metric::Avg), &HashMap::new(), &[]).unwrap();

        assert_eq!(data.series[0].values, vec![Some(100.0)]);
        assert_eq!(data.skipped_no_value, 1);
        assert_eq!(data.charted, 1);
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        let values: Vec<f64> = (1..=100).map(|v| v as f64).collect();
        assert_eq!(percentile(&values, 0.95), Some(95.0));
        assert_eq!(percentile(&values, 0.50), Some(50.0));
        assert_eq!(percentile(&[], 0.95), None);
        assert_eq!(percentile(&[7.0], 0.99), Some(7.0));
    }

    #[test]
    fn category_mode_ranks_bars_by_the_metric_and_cuts_to_top_n() {
        let mut hits = vec![
            hit("a", Some("2026-07-14T10:00:00"), "slow", Some(900.0)),
            hit("a", Some("2026-07-14T10:01:00"), "medium", Some(500.0)),
            hit("a", Some("2026-07-14T10:02:00"), "fast", Some(10.0)),
        ];
        hits[0].fields.operation = Some("SlowOp".into());
        hits[1].fields.operation = Some("MediumOp".into());
        hits[2].fields.operation = Some("FastOp".into());

        let mut req = request(ChartMode::Category, GroupBy::Operation, Metric::Max);
        req.top_n = 2;
        let data = aggregate(&hits, &req, &HashMap::new(), &[]).unwrap();

        assert_eq!(data.labels, vec!["SlowOp", "MediumOp"]);
        assert_eq!(data.series.len(), 1);
        assert_eq!(data.series[0].values, vec![Some(900.0), Some(500.0)]);
        assert_eq!(data.other_groups, 1, "FastOp did not make the cut");
    }

    #[test]
    fn custom_regex_groups_by_key_and_aggregates_the_captured_number() {
        let hits = vec![
            hit("a", Some("2026-07-14T10:00:00"), "GET /orders took 120 ms", None),
            hit("a", Some("2026-07-14T10:01:00"), "GET /orders took 180 ms", None),
            hit("a", Some("2026-07-14T10:02:00"), "GET /users took 40 ms", None),
            hit("a", Some("2026-07-14T10:03:00"), "unrelated line", None),
        ];
        let mut req = request(ChartMode::Category, GroupBy::Custom, Metric::Avg);
        req.custom_regex = Some(r"GET (?<key>\S+) took (?<value>\d+) ms".into());
        req.value_source = ValueSource::Custom;
        let data = aggregate(&hits, &req, &HashMap::new(), &[]).unwrap();

        assert_eq!(data.labels, vec!["/orders", "/users"]);
        assert_eq!(data.series[0].values, vec![Some(150.0), Some(40.0)]);
        assert_eq!(data.skipped_no_key, 1, "the unrelated line has no key");
    }

    #[test]
    fn custom_regex_falls_back_to_positional_groups() {
        let hits = vec![hit("a", Some("2026-07-14T10:00:00"), "op=Sync ms=250", None)];
        let mut req = request(ChartMode::Category, GroupBy::Custom, Metric::Sum);
        req.custom_regex = Some(r"op=(\w+) ms=(\d+)".into());
        req.value_source = ValueSource::Custom;
        let data = aggregate(&hits, &req, &HashMap::new(), &[]).unwrap();

        assert_eq!(data.labels, vec!["Sync"]);
        assert_eq!(data.series[0].values, vec![Some(250.0)]);
    }

    #[test]
    fn custom_grouping_without_a_regex_is_an_error() {
        let req = request(ChartMode::Category, GroupBy::Custom, Metric::Count);
        assert!(aggregate(&[], &req, &HashMap::new(), &[]).is_err());
    }

    #[test]
    fn undatable_hits_are_skipped_on_a_timeline_but_counted_in_a_category() {
        let hits = vec![
            hit("a", Some("2026-07-14T10:00:00"), "boom", None),
            hit("a", None, "continuation line", None),
        ];
        let timeline = aggregate(&hits, &request(ChartMode::Timeline, GroupBy::None, Metric::Count), &HashMap::new(), &[]).unwrap();
        assert_eq!(timeline.skipped_no_time, 1);
        assert_eq!(timeline.charted, 1);

        let category = aggregate(&hits, &request(ChartMode::Category, GroupBy::None, Metric::Count), &HashMap::new(), &[]).unwrap();
        assert_eq!(category.skipped_no_time, 0);
        assert_eq!(category.charted, 2);
    }

    #[test]
    fn auto_bucket_keeps_the_point_count_sane() {
        assert_eq!(auto_bucket(120), 1, "two minutes -> per second");
        assert_eq!(auto_bucket(3600), 60, "an hour -> per minute");
        assert_eq!(auto_bucket(86400), 900, "a day -> quarter hours");
        assert_eq!(auto_bucket(30 * 86400), 21600, "a month -> six hours");
    }

    #[test]
    fn too_many_buckets_is_a_readable_error_not_a_giant_payload() {
        let hits = vec![
            hit("a", Some("2026-01-01T00:00:00"), "boom", None),
            hit("a", Some("2026-07-14T00:00:00"), "boom", None),
        ];
        let mut req = request(ChartMode::Timeline, GroupBy::None, Metric::Count);
        req.bucket = Bucket::Minute;
        let error = aggregate(&hits, &req, &HashMap::new(), &[]).unwrap_err();
        assert!(error.contains("coarser bucket"), "{error}");
    }

    #[test]
    fn matched_groups_chart_one_series_per_group_in_pattern_order() {
        let names = vec!["err".to_string(), "warn".to_string(), "silent".to_string()];
        let mut hits = vec![
            hit("a", Some("2026-07-14T10:00:00"), "WARN thing", None),
            hit("a", Some("2026-07-14T10:01:00"), "ERROR boom", None),
            hit("a", Some("2026-07-14T10:02:00"), "WARN then ERROR", None),
        ];
        hits[0].matched_groups = vec!["warn".into()];
        hits[1].matched_groups = vec!["err".into()];
        hits[2].matched_groups = vec!["err".into(), "warn".into()];

        let req = request(ChartMode::Timeline, GroupBy::MatchedGroup, Metric::Count);
        let data = aggregate(&hits, &req, &HashMap::new(), &names).unwrap();

        // Pattern order, not count order; a group with no hits still shows
        let series_names: Vec<&str> = data.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(series_names, ["err", "warn", "silent"]);
        assert_eq!(data.series[0].values, vec![Some(2.0)]);
        assert_eq!(data.series[1].values, vec![Some(2.0)], "a hit counts in every group it matched");
        assert_eq!(data.series[2].values, vec![Some(0.0)]);
        assert_eq!(data.charted, 3, "hits, not (hit, group) pairs");
    }

    #[test]
    fn matched_groups_without_any_groups_in_the_query_is_an_error() {
        let req = request(ChartMode::Timeline, GroupBy::MatchedGroup, Metric::Count);
        let error = aggregate(&[], &req, &HashMap::new(), &[]).unwrap_err();
        assert!(error.contains("capture groups"), "{error}");
    }

    #[test]
    fn hits_that_matched_no_group_are_reported_as_skipped() {
        let names = vec!["err".to_string()];
        let mut hits = vec![
            hit("a", Some("2026-07-14T10:00:00"), "ERROR boom", None),
            hit("a", Some("2026-07-14T10:01:00"), "context line", None),
        ];
        hits[0].matched_groups = vec!["err".into()];

        let req = request(ChartMode::Timeline, GroupBy::MatchedGroup, Metric::Count);
        let data = aggregate(&hits, &req, &HashMap::new(), &names).unwrap();
        assert_eq!(data.skipped_no_key, 1);
        assert_eq!(data.charted, 1);
    }

    #[test]
    fn empty_result_charts_as_nothing_rather_than_failing() {
        let data = aggregate(&[], &request(ChartMode::Timeline, GroupBy::None, Metric::Count), &HashMap::new(), &[]).unwrap();
        assert!(data.labels.is_empty());
        assert!(data.series.is_empty());
        assert_eq!(data.charted, 0);
    }
}
