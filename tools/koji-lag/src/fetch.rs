// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Talking to a Koji hub: resolving a window, pacing requests, fetching
//! child tasks, and turning hub tasks into records.
//!
//! What orchestrates all this is [`crate::sync`]; this module is the
//! plumbing it uses. It no longer walks history itself — that moved to a
//! cursor over creation time in [`crate::sweep`], because walking by page
//! offset could not reach far history at all.

use sandogasa_kojihub::hub::{HubClient, ListTasksOpts};
use sandogasa_kojihub::{Value, retry};

use crate::dataset::{BuildRecord, TaskRecord};

/// Parse a `YYYY-MM-DD` CLI date to UTC-midnight unix seconds.
pub fn date_to_ts(date: &str) -> Result<f64, String> {
    let parsed = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|e| format!("invalid date '{date}': {e}"))?;
    Ok(parsed
        .and_hms_opt(0, 0, 0)
        .expect("midnight exists")
        .and_utc()
        .timestamp() as f64)
}

/// Start of the current UTC day (unix days are exactly 86400s).
fn utc_day_start(now: f64) -> f64 {
    (now / 86_400.0).floor() * 86_400.0
}

/// Resolve the CLI window flags to a completion-time
/// half-open window `[after, before)` (UTC unix seconds).
/// Windows cover **whole UTC days**
/// only: `--days N` means the last N *complete* days, and a
/// dateless upper bound stops at today's 00:00 UTC — never the
/// partial current day — so periodic "a few days at a time"
/// fetches compose seamlessly. An explicit `--until DATE`
/// includes that whole day (clamped to `now` if it's today).
///
/// The window selects builds by **completion** time: a build
/// still running has no timing to report yet and is picked up by
/// whichever fetch covers the day it finishes, so every build is
/// counted exactly once.
pub fn resolve_window(
    since: Option<&str>,
    until: Option<&str>,
    days: Option<u32>,
    now: f64,
) -> Result<(f64, f64), String> {
    let today = utc_day_start(now);
    let after = match (since, days) {
        (Some(date), _) => date_to_ts(date)?,
        (None, Some(days)) => today - f64::from(days) * 86_400.0,
        (None, None) => {
            return Err("a window lower bound is required: --since or --days".to_string());
        }
    };
    let before = match until {
        // Inclusive end date: up to midnight of the following day.
        Some(date) => (date_to_ts(date)? + 86_400.0).min(now),
        // Only complete days by default.
        None => today,
    };
    if before <= after {
        return Err(
            "the window is empty — it covers complete UTC days only, so \
             fetching today's builds needs an explicit --until with \
             today's date"
                .to_string(),
        );
    }
    Ok((after, before))
}

/// Everything a fetch needs, resolved by the CLI layer.
pub struct FetchOpts {
    pub instance_key: String,
    pub hub_url: String,
    /// Completion-window bounds, UTC unix seconds.
    pub after: f64,
    pub before: f64,
    pub page_size: i64,
    pub sleep_ms: u64,
    pub retries: u32,
    /// Share of one connection to aim for; see [`Pace`].
    pub duty_percent: u32,
    pub verbose: bool,
}

impl FetchOpts {
    pub(crate) fn pace(&self) -> Pace {
        Pace {
            percent: self.duty_percent,
            floor: std::time::Duration::from_millis(self.sleep_ms),
        }
    }
}

/// How far past the window start the id walk keeps going, to
/// catch builds created before the window that completed inside
/// it. Three days comfortably exceeds any real build duration
/// (chromium on s390x included) at the cost of a few extra pages.
pub const CREATE_GRACE_SECS: f64 = 3.0 * 86_400.0;

/// Parents per child-fetch batch.
///
/// Almost all of a batch's cost is the round trip, not the rows: measured
/// against Fedora's hub, 40 parents cost 28ms each, 100 cost 5.5ms, 200
/// cost 3.8ms and 400 cost 4.2ms. Since a build has four children on
/// average, the flat part dominates until a batch is in the hundreds —
/// and this is the expensive half of a sync, so the difference is a day's
/// children in a minute rather than eight.
///
/// 200 rather than 400 because a batch whose answer fills the page has to
/// be split and refetched: at 200 the response is around 800 rows, which
/// leaves [`CHILD_PAGE_LIMIT`] several times the headroom an arch-heavy
/// build needs.
pub(crate) const PARENT_CHUNK: usize = 200;

/// Rows one child batch may return.
///
/// Not `--page-size`, which sizes the build listing: a batch of parents
/// answers with several times as many rows as it has parents, and tying
/// the two together would make a larger listing page silently raise the
/// overflow threshold for children.
const CHILD_PAGE_LIMIT: i64 = 5_000;

/// How much of one connection's capacity a sweep may use.
///
/// A fixed pause between requests paces backwards under load: at 500ms
/// between half-second queries the hub sees us half the time, but when it
/// is struggling and the same query takes eight seconds we occupy it 94%
/// of the time — leaning hardest exactly when we should ease off. Pausing
/// in proportion to how long the last request took inverts that, and it
/// speeds up again by itself when the hub does.
///
/// `percent` is the share of one connection to aim for: 50 means pause as
/// long as the request took. `floor` applies regardless, so quick answers
/// do not become a tight loop.
#[derive(Debug, Clone, Copy)]
pub struct Pace {
    pub percent: u32,
    pub floor: std::time::Duration,
}

impl Pace {
    /// How long to wait after a request that took `latency`.
    pub fn after(&self, latency: std::time::Duration) -> std::time::Duration {
        let percent = self.percent.clamp(1, 100);
        // duty = latency / (latency + wait), so wait = latency * (100 - duty) / duty.
        let wait = latency.mul_f64((100 - percent) as f64 / percent as f64);
        wait.max(self.floor)
    }

    /// Wait, having just spent `latency` on a request.
    pub fn rest(&self, latency: std::time::Duration) {
        std::thread::sleep(self.after(latency));
    }
}

/// Progress through the per-parent sweep of `buildArch` children.
///
/// Unlike the build walk, the size of this job *is* known in advance —
/// the parents were counted by the walk — so progress is reported
/// against it. Batches alone would not do: a batch whose answer fills a
/// page is split in two and re-queued, so the batch count can rise
/// without any parent being finished, and a reader watching only batches
/// would see motion without progress. Parents are therefore counted as
/// done when a batch is accepted, and a split says so.
#[derive(Debug)]
pub struct BatchProgress {
    total_parents: usize,
    done_parents: usize,
    chunk: usize,
    pub batches: usize,
}

impl BatchProgress {
    pub fn new(total_parents: usize, chunk: usize) -> Self {
        Self {
            total_parents,
            done_parents: 0,
            chunk: chunk.max(1),
            batches: 0,
        }
    }

    /// Record a batch and describe the position. `split` means its
    /// answer overflowed the page, so it will be retried in halves and
    /// its parents are not done.
    pub fn note(&mut self, parents: usize, tasks: usize, split: bool) -> String {
        self.batches += 1;
        if !split {
            self.done_parents += parents;
        }
        let mut line = format!(
            "batch {} ({parents} parent(s), {tasks} task(s))",
            self.batches
        );
        if split {
            line += " — too many children for one page, splitting";
            return line;
        }
        let left = self.total_parents.saturating_sub(self.done_parents);
        let pct = match self.total_parents {
            0 => 100.0,
            total => self.done_parents as f64 / total as f64 * 100.0,
        };
        line += &format!(
            " — {} of {} parent(s), {pct:.0}%, ~{} batch(es) to go",
            self.done_parents,
            self.total_parents,
            left.div_ceil(self.chunk)
        );
        line
    }
}

/// What a caller does with each batch of children: store them, and take
/// the parents they settled as done.
pub(crate) type OnBatch<'a> =
    &'a mut dyn FnMut(&[i64], Vec<sandogasa_kojihub::HubTask>) -> Result<(), String>;

/// [`fetch_children`], handing each accepted batch to `on_batch` instead
/// of collecting the lot.
///
/// A sweep that stores and marks per batch keeps its progress when
/// interrupted, which matters because this is the expensive half of a
/// sync: several hundred queries for a day. `on_batch` is given the
/// parents the batch settled — after any splitting, so every id in it has
/// had its children returned in full.
pub(crate) fn fetch_children_batched(
    hub: &HubClient,
    parents: &[i64],
    opts: &FetchOpts,
    on_batch: OnBatch<'_>,
) -> Result<(), String> {
    let mut chunks: Vec<Vec<i64>> = parents.chunks(PARENT_CHUNK).map(<[i64]>::to_vec).collect();
    let mut progress = BatchProgress::new(parents.len(), PARENT_CHUNK);
    while let Some(chunk) = chunks.pop() {
        // No method filter: one query per batch returns every child, and
        // the SRPM rebuild that precedes the per-arch builds is wanted
        // too — its own scheduling decides part of a build's wall clock.
        // Children this tool has nothing to say about (tagBuild,
        // buildNotification) are dropped below.
        let list_opts = ListTasksOpts {
            parent: Some(chunk.clone()),
            decode: true,
            ..Default::default()
        };
        let query = sandogasa_kojihub::QueryOpts {
            limit: Some(CHILD_PAGE_LIMIT),
            ..Default::default()
        };
        let started = std::time::Instant::now();
        let page = retry(opts.retries, || hub.list_tasks(&list_opts, &query))
            .map_err(|e| format!("listTasks(parent batch) failed: {e}"))?;
        let latency = started.elapsed();
        // Whether this batch stands decides what the line may claim, so
        // it is settled before the line is written.
        let overflowed = (page.len() as i64) >= CHILD_PAGE_LIMIT;
        let splitting = overflowed && chunk.len() > 1;
        let where_it_is = progress.note(chunk.len(), page.len(), splitting);
        if opts.verbose {
            eprintln!("[koji-lag] children: {where_it_is}");
        }
        opts.pace().rest(latency);
        if splitting {
            let mid = chunk.len() / 2;
            chunks.push(chunk[..mid].to_vec());
            chunks.push(chunk[mid..].to_vec());
            continue;
        }
        // Only the two methods that make up a build's wall clock. The
        // rest of a build's children (tagging, notification) say nothing
        // about how long the machines took.
        let page: Vec<_> = page
            .into_iter()
            .filter(|t| {
                t.method == crate::dataset::BUILD_ARCH || crate::dataset::is_srpm_step(&t.method)
            })
            .collect();
        if overflowed {
            eprintln!(
                "warning: build task {} has {}+ child tasks; some may be missed",
                chunk[0], CHILD_PAGE_LIMIT
            );
        }
        on_batch(&chunk, page)?;
    }
    Ok(())
}

/// Extract the source package name from a decoded task request:
/// the first string element that looks like an SRPM path.
pub fn package_from_request(request: &Value) -> Option<String> {
    let nvr = nvr_from_request(request)?;
    sandogasa_koji::parse_nvr(&nvr).map(|(name, _, _)| name.to_string())
}

/// Extract the NVR (basename minus `.src.rpm`) from a decoded
/// request. `build` requests may carry a git URL instead — those
/// return `None` and the caller falls back to the buildArch
/// child's srpm.
pub fn nvr_from_request(request: &Value) -> Option<String> {
    let first = request.as_array()?.first()?.as_str()?;
    let basename = first.rsplit('/').next()?;
    let nvr = basename.strip_suffix(".src.rpm")?;
    if nvr.is_empty() {
        None
    } else {
        Some(nvr.to_string())
    }
}

/// Extract the build target (second positional string) from a
/// `build` request.
fn target_from_request(request: &Value) -> Option<String> {
    request.as_array()?.get(1)?.as_str().map(str::to_string)
}

/// Whether a `build` request's opts struct sets `scratch`. The
/// request layout is positional and loosely specified, so scan
/// for the first struct member defensively; absent means a
/// regular build (undercounting scratch, never miscounting
/// official).
pub fn scratch_from_request(request: &Value) -> bool {
    let Some(items) = request.as_array() else {
        return false;
    };
    items
        .iter()
        .find_map(|item| item.as_struct().map(|_| item))
        .and_then(|opts| opts.get("scratch"))
        .and_then(|v| match v {
            Value::Boolean(b) => Some(*b),
            _ => None,
        })
        .unwrap_or(false)
}

pub(crate) fn build_record(instance: &str, task: &sandogasa_kojihub::HubTask) -> BuildRecord {
    let (package, nvr) = match &task.request {
        Some(req) => {
            let nvr = nvr_from_request(req);
            let package = nvr
                .as_deref()
                .and_then(|n| sandogasa_koji::parse_nvr(n).map(|(name, _, _)| name.to_string()));
            (package, nvr)
        }
        None => (None, None),
    };
    BuildRecord {
        instance: instance.to_string(),
        task_id: task.id,
        package,
        nvr,
        target: task.request.as_ref().and_then(target_from_request),
        owner: task.owner_name.clone(),
        scratch: task.request.as_ref().is_some_and(scratch_from_request),
        state: task.state,
        create_ts: task.create_ts.unwrap_or(0.0),
        start_ts: task.start_ts,
        completion_ts: task.completion_ts,
        priority: task.priority,
        host_id: task.host_id,
    }
}

/// Convert a buildArch task; `None` (with a warning) when the
/// record is unusable for lag analysis.
pub(crate) fn task_record(instance: &str, task: &sandogasa_kojihub::HubTask) -> Option<TaskRecord> {
    let Some(arch) = task.arch.clone() else {
        eprintln!(
            "warning: {} task {} has no arch; skipped",
            task.method, task.id
        );
        return None;
    };
    let Some(create_ts) = task.create_ts else {
        eprintln!(
            "warning: {} task {} has no create_ts; skipped",
            task.method, task.id
        );
        return None;
    };
    Some(TaskRecord {
        instance: instance.to_string(),
        task_id: task.id,
        parent: task.parent,
        arch,
        method: task.method.clone(),
        package: task.request.as_ref().and_then(package_from_request),
        state: task.state,
        create_ts,
        start_ts: task.start_ts,
        completion_ts: task.completion_ts,
        host_id: task.host_id,
        channel_id: task.channel_id,
        weight: task.weight,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn pacing_follows_how_slow_the_hub_is() {
        use std::time::Duration;
        let pace = Pace {
            percent: 50,
            floor: Duration::from_millis(500),
        };
        // Half of one connection: pause as long as the request took.
        assert_eq!(pace.after(Duration::from_secs(8)), Duration::from_secs(8));
        // A hub that answers quickly is asked again sooner, down to the
        // floor — so throughput rises by itself when load drops.
        assert_eq!(
            pace.after(Duration::from_millis(100)),
            Duration::from_millis(500)
        );

        // A gentler share waits proportionally longer.
        let quarter = Pace {
            percent: 25,
            floor: Duration::from_millis(0),
        };
        assert_eq!(
            quarter.after(Duration::from_secs(2)),
            Duration::from_secs(6)
        );

        // The whole connection: only the floor applies.
        let flat = Pace {
            percent: 100,
            floor: Duration::from_millis(200),
        };
        assert_eq!(
            flat.after(Duration::from_secs(5)),
            Duration::from_millis(200)
        );
    }

    #[test]
    fn batch_progress_counts_parents_not_batches() {
        let mut progress = BatchProgress::new(100, 40);

        let first = progress.note(40, 812, false);
        assert!(
            first.contains("batch 1 (40 parent(s), 812 task(s))"),
            "{first}"
        );
        assert!(first.contains("40 of 100 parent(s), 40%"), "{first}");
        assert!(first.contains("~2 batch(es) to go"), "{first}");

        // A batch whose answer overflowed is retried in halves, so no
        // parent finished and the line says why rather than implying
        // progress.
        let split = progress.note(40, 1000, true);
        assert!(
            split.contains("batch 2 (40 parent(s), 1000 task(s))"),
            "{split}"
        );
        assert!(split.contains("splitting"), "{split}");
        assert!(!split.contains('%'), "{split}");

        // The halves then land.
        let half = progress.note(20, 400, false);
        assert!(half.contains("60 of 100 parent(s), 60%"), "{half}");
        let rest = progress.note(20, 400, false);
        assert!(rest.contains("80 of 100 parent(s), 80%"), "{rest}");
        assert_eq!(progress.batches, 4);
    }

    fn value_str(s: &str) -> Value {
        Value::String(s.to_string())
    }

    /// 2026-07-20 15:00 UTC (8 AM US Pacific): the user's worked
    /// example — the last scanned day must be July 19.
    const MID_JULY_20: f64 = 1_784_559_600.0;
    const JULY_20_MIDNIGHT: f64 = 1_784_505_600.0;

    #[test]
    fn days_cover_whole_utc_days_ending_yesterday() {
        // --days 1 run mid-day on July 20 scans exactly July 19.
        let (after, before) = resolve_window(None, None, Some(1), MID_JULY_20).unwrap();
        assert_eq!(before, JULY_20_MIDNIGHT);
        assert_eq!(after, JULY_20_MIDNIGHT - 86_400.0);
        // --days 3: July 17 through 19.
        let (after, _) = resolve_window(None, None, Some(3), MID_JULY_20).unwrap();
        assert_eq!(after, JULY_20_MIDNIGHT - 3.0 * 86_400.0);
    }

    #[test]
    fn since_without_until_also_stops_at_the_last_complete_day() {
        let (after, before) = resolve_window(Some("2026-07-15"), None, None, MID_JULY_20).unwrap();
        assert_eq!(after, JULY_20_MIDNIGHT - 5.0 * 86_400.0);
        assert_eq!(before, JULY_20_MIDNIGHT);
    }

    #[test]
    fn explicit_until_includes_that_day_clamped_to_now() {
        // A past end date covers through its full day.
        let (_, before) =
            resolve_window(Some("2026-07-15"), Some("2026-07-18"), None, MID_JULY_20).unwrap();
        assert_eq!(before, JULY_20_MIDNIGHT - 86_400.0);
        // Today's date opts into the partial running day.
        let (_, before) =
            resolve_window(Some("2026-07-15"), Some("2026-07-20"), None, MID_JULY_20).unwrap();
        assert_eq!(before, MID_JULY_20);
    }

    #[test]
    fn empty_and_invalid_windows_error() {
        // --since today with no --until: no complete day yet.
        let err = resolve_window(Some("2026-07-20"), None, None, MID_JULY_20).unwrap_err();
        assert!(err.contains("complete UTC days"), "{err}");
        assert!(resolve_window(None, None, None, MID_JULY_20).is_err());
        assert!(resolve_window(Some("garbage"), None, None, MID_JULY_20).is_err());
    }

    #[test]
    fn package_and_nvr_from_srpm_path() {
        let req = Value::Array(vec![
            value_str("tasks/8163/148158163/rabbitmq-server-4.3.3-2.fc45.src.rpm"),
            Value::Int(128157),
            value_str("ppc64le"),
        ]);
        assert_eq!(
            nvr_from_request(&req).as_deref(),
            Some("rabbitmq-server-4.3.3-2.fc45")
        );
        assert_eq!(
            package_from_request(&req).as_deref(),
            Some("rabbitmq-server")
        );
    }

    #[test]
    fn bare_srpm_filename_works() {
        let req = Value::Array(vec![value_str("foo-1.0-1.fc45.src.rpm")]);
        assert_eq!(package_from_request(&req).as_deref(), Some("foo"));
    }

    #[test]
    fn git_url_request_yields_none() {
        let req = Value::Array(vec![value_str(
            "git+https://src.fedoraproject.org/rpms/foo.git#deadbeef",
        )]);
        assert_eq!(nvr_from_request(&req), None);
        assert_eq!(package_from_request(&req), None);
    }

    #[test]
    fn garbage_requests_yield_none() {
        assert_eq!(package_from_request(&Value::Nil), None);
        assert_eq!(package_from_request(&Value::Array(vec![])), None);
        assert_eq!(
            package_from_request(&Value::Array(vec![value_str(".src.rpm")])),
            None
        );
    }

    #[test]
    fn scratch_detection_scans_for_the_opts_struct() {
        let mut opts = HashMap::new();
        opts.insert("scratch".to_string(), Value::Boolean(true));
        let req = Value::Array(vec![
            value_str("git+https://src.fedoraproject.org/rpms/foo.git#abc"),
            value_str("f45-candidate"),
            Value::Struct(opts),
        ]);
        assert!(scratch_from_request(&req));

        let mut no_scratch = HashMap::new();
        no_scratch.insert("repo_id".to_string(), Value::Int(1));
        let req = Value::Array(vec![value_str("x.src.rpm"), Value::Struct(no_scratch)]);
        assert!(!scratch_from_request(&req));
        assert!(!scratch_from_request(&Value::Array(vec![value_str("x")])));
    }

    #[test]
    fn target_is_the_second_string() {
        let req = Value::Array(vec![
            value_str("git+https://src.fedoraproject.org/rpms/foo.git#abc"),
            value_str("f45-candidate"),
        ]);
        assert_eq!(target_from_request(&req).as_deref(), Some("f45-candidate"));
    }
}
