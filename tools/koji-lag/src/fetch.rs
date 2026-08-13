// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The `fetch` subcommand: sweep a completion-time window of
//! build/buildArch tasks from a Koji hub into the local dataset.
//!
//! Strategy (shaped by live measurements against a loaded hub,
//! where even five-minute completion-window filters timed out):
//! no completion filters at all. The parent `build` tasks are
//! found by walking `listTasks` pages newest-first by task id (an
//! index walk, ~1.3s per 500 decoded rows under the same load)
//! until pages predate the window minus a grace margin for
//! long-running builds; the window is then applied client-side on
//! completion time. The per-arch `buildArch` children come from
//! parent-batched queries, which hit koji's `task(parent)` index
//! (~0.5s). The window's upper bound is frozen once at sweep
//! start, so builds completing mid-sweep can't shift the result
//! set. Sweeps are single-threaded and paced (`--sleep-ms`
//! between requests) out of politeness to the hub.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::Utc;
use sandogasa_kojihub::hub::{HubClient, ListTasksOpts};
use sandogasa_kojihub::{Value, retry};

use crate::dataset::{BuildRecord, Dataset, FetchWindow, TaskRecord};

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
    /// Keep only builds submitted by this user.
    pub owner: Option<String>,
    /// Keep only these source packages.
    pub packages: Option<BTreeSet<String>>,
    pub page_size: i64,
    pub sleep_ms: u64,
    pub retries: u32,
    pub verbose: bool,
}

impl FetchOpts {
    /// Whether this fetch is scoped (not full coverage).
    fn filtered(&self) -> bool {
        self.owner.is_some() || self.packages.is_some()
    }
}

/// How far past the window start the id walk keeps going, to
/// catch builds created before the window that completed inside
/// it. Three days comfortably exceeds any real build duration
/// (chromium on s390x included) at the cost of a few extra pages.
const CREATE_GRACE_SECS: f64 = 3.0 * 86_400.0;

/// Parents per child-fetch batch: ~5 arches per build keeps the
/// response comfortably under any page size.
const PARENT_CHUNK: usize = 40;

/// Progress through the newest-first walk of `build` tasks.
///
/// A page number alone says nothing: "page 219" gives no idea whether
/// that is nearly done or barely started, and a busy month runs to
/// hundreds of pages. What the walk is actually doing is marching
/// backwards in time towards the start of the window, so how far back it
/// has reached is the honest measure, and it is already in hand — every
/// task on a page carries its creation time.
///
/// The remaining pages are estimated from the density observed so far
/// (tasks per second of history) rather than from a count query. Asking
/// the hub how many tasks a window holds means a filtered count, which
/// measured 83 seconds against Fedora's hub for a three-day window —
/// the same index problem that rules out server-side completion
/// filtering. So the estimate is free, marked with a `~`, and improves
/// with every page. It assumes tasks are spread evenly, which they are
/// not: weekends and mass rebuilds skew it, and a jump is the walk
/// learning rather than a fault.
#[derive(Debug)]
pub struct WalkProgress {
    /// Where the walk stops: the oldest creation time it needs.
    target_ts: f64,
    /// Newest creation time seen, from the first page.
    newest_ts: Option<f64>,
    /// Oldest creation time seen so far.
    oldest_ts: Option<f64>,
    pub pages: usize,
    pub tasks: usize,
    page_size: usize,
}

impl WalkProgress {
    pub fn new(target_ts: f64, page_size: usize) -> Self {
        Self {
            target_ts,
            newest_ts: None,
            oldest_ts: None,
            pages: 0,
            tasks: 0,
            page_size: page_size.max(1),
        }
    }

    /// Record a page and describe where the walk has got to.
    pub fn note(&mut self, created: impl IntoIterator<Item = f64>, len: usize) -> String {
        self.pages += 1;
        self.tasks += len;
        for ts in created {
            self.newest_ts = Some(self.newest_ts.map_or(ts, |n: f64| n.max(ts)));
            self.oldest_ts = Some(self.oldest_ts.map_or(ts, |o: f64| o.min(ts)));
        }
        let Some((newest, oldest)) = self.newest_ts.zip(self.oldest_ts) else {
            return format!("page {} ({} task(s))", self.pages, len);
        };
        let day = |ts: f64| {
            chrono::DateTime::from_timestamp(ts as i64, 0)
                // The hour matters: inside a short window the date
                // never changes, and a line that never changes reads as
                // a walk that is not moving.
                .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "?".to_string())
        };
        let whole = newest - self.target_ts;
        let covered = newest - oldest;
        let mut line = format!(
            "page {} ({} task(s), {} so far), back to {}",
            self.pages,
            len,
            self.tasks,
            day(oldest)
        );
        if whole > 0.0 && covered > 0.0 {
            let done = (covered / whole * 100.0).min(100.0);
            // Density so far, projected over what is left. Only worth
            // saying while there is something left to project.
            let left = (whole - covered).max(0.0);
            let rate = self.tasks as f64 / covered;
            let pages_left = (rate * left / self.page_size as f64).ceil() as usize;
            line += &format!(" — {done:.0}% of the window, ~{pages_left} page(s) to go");
        }
        line
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

/// Counts for the CLI summary line.
#[derive(Debug, Default)]
pub struct FetchReport {
    pub tasks_swept: usize,
    pub builds_swept: usize,
    pub records_added: usize,
    pub records_replaced: usize,
}

/// Run a fetch into the dataset at `out_path` (created if
/// missing, merged into if present).
pub fn run(opts: &FetchOpts, out_path: &Path) -> Result<FetchReport, String> {
    // Cheap preconditions first: an unreadable/unwritable dataset
    // or an unreachable hub must fail in seconds, not after a long
    // sweep.
    let mut dataset = if out_path.exists() {
        Dataset::load(out_path)?
    } else {
        Dataset::new()
    };
    dataset.save(out_path)?;

    let hub = HubClient::new(&opts.hub_url);
    let hosts = retry(opts.retries, || hub.list_hosts_with_arches())
        .map_err(|e| format!("cannot reach the hub at {}: {e}", opts.hub_url))?;
    let channels =
        retry(opts.retries, || hub.list_channels()).map_err(|e| format!("listChannels: {e}"))?;
    for (id, name, arches) in hosts {
        let key = format!("{}:{id}", opts.instance_key);
        dataset.hosts.insert(key.clone(), name);
        if !arches.is_empty() {
            dataset.host_arches.insert(key, arches);
        }
    }
    for (id, name) in channels {
        dataset
            .channels
            .insert(format!("{}:{id}", opts.instance_key), name);
    }

    let mut report = FetchReport::default();

    // Find the parent `build` tasks by walking newest-first and
    // windowing on completion time client-side (no server-side
    // completion filter — see the module docs). A failure
    // mid-walk still merges what was fetched (without recording
    // the coverage window — coverage must not be overclaimed) so
    // a re-run resumes instead of starting over.
    let list_opts = ListTasksOpts {
        method: Some("build".to_string()),
        decode: true,
        ..Default::default()
    };
    let mut progress = WalkProgress::new(
        opts.after - CREATE_GRACE_SECS,
        opts.page_size.max(1) as usize,
    );
    let mut on_page = |page: &[sandogasa_kojihub::HubTask]| {
        let created = page.iter().filter_map(|t| t.create_ts);
        let where_it_is = progress.note(created, page.len());
        if opts.verbose {
            eprintln!("[koji-lag] build walk: {where_it_is}");
        }
        std::thread::sleep(std::time::Duration::from_millis(opts.sleep_ms));
    };
    let build_tasks = match hub.walk_tasks_desc(
        &list_opts,
        opts.page_size,
        opts.retries,
        opts.after - CREATE_GRACE_SECS,
        &mut on_page,
    ) {
        Ok(tasks) => tasks
            .into_iter()
            .filter(|t| {
                t.completion_ts
                    .is_some_and(|ts| ts >= opts.after && ts < opts.before)
            })
            .collect::<Vec<_>>(),
        Err(e) => {
            dataset.save(out_path)?;
            return Err(format!(
                "listTasks(build) walk failed: {e}\n\
                 (partial data saved; re-run to resume)"
            ));
        }
    };
    report.builds_swept = build_tasks.len();

    let mut incoming = Dataset::new();
    let mut parent_packages: BTreeMap<i64, Option<String>> = BTreeMap::new();
    for task in &build_tasks {
        let record = build_record(&opts.instance_key, task);
        parent_packages.insert(task.id, record.package.clone());
        incoming.builds.insert(record.key(), record);
    }

    // Fetch the buildArch children per parent batch — an indexed
    // lookup that stays fast even when completion filtering is
    // slow. Every child has its parent by construction.
    let parent_ids: Vec<i64> = build_tasks.iter().map(|t| t.id).collect();
    let arch_tasks = match fetch_children(&hub, &parent_ids, opts) {
        Ok(t) => t,
        Err(e) => {
            dataset.save(out_path)?;
            return Err(format!("{e}\n(partial data saved; re-run to resume)"));
        }
    };
    report.tasks_swept = arch_tasks.len();
    for task in &arch_tasks {
        if let Some(record) = task_record(&opts.instance_key, task) {
            incoming.tasks.insert(record.key(), record);
        }
    }

    // Inherit the package name from the parent where the child's
    // own request didn't carry a parseable srpm.
    for record in incoming.tasks.values_mut() {
        if record.package.is_none()
            && let Some(parent) = record.parent
            && let Some(Some(pkg)) = parent_packages.get(&parent)
        {
            record.package = Some(pkg.clone());
        }
    }

    apply_filters(&mut incoming, opts);

    incoming.meta.windows.push(FetchWindow {
        instance: opts.instance_key.clone(),
        from: opts.after,
        to: opts.before,
        fetched: Utc::now(),
        filtered: opts.filtered(),
    });

    let stats = dataset.merge(incoming);
    report.records_added = stats.added;
    report.records_replaced = stats.replaced;
    dataset.save(out_path)?;
    Ok(report)
}

/// Fetch the `buildArch` children of `parents` in chunks via the
/// indexed parent filter. A chunk whose response fills the page
/// may be truncated, so it splits in half and refetches; a single
/// parent with a full page is accepted with a warning (a build
/// with `page_size` arch tasks does not exist in practice).
fn fetch_children(
    hub: &HubClient,
    parents: &[i64],
    opts: &FetchOpts,
) -> Result<Vec<sandogasa_kojihub::HubTask>, String> {
    let mut all = Vec::new();
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
            limit: Some(opts.page_size),
            ..Default::default()
        };
        let page = retry(opts.retries, || hub.list_tasks(&list_opts, &query))
            .map_err(|e| format!("listTasks(buildArch, parent batch) failed: {e}"))?;
        // Whether this batch stands decides what the line may claim, so
        // it is settled before the line is written.
        let overflowed = (page.len() as i64) >= opts.page_size;
        let splitting = overflowed && chunk.len() > 1;
        let where_it_is = progress.note(chunk.len(), page.len(), splitting);
        if opts.verbose {
            eprintln!("[koji-lag] children: {where_it_is}");
        }
        std::thread::sleep(std::time::Duration::from_millis(opts.sleep_ms));
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
            .filter(|t| matches!(t.method.as_str(), "buildArch" | "rebuildSRPM"))
            .collect();
        if overflowed {
            eprintln!(
                "warning: build task {} has {}+ child tasks; some may be missed",
                chunk[0], opts.page_size
            );
        }
        all.extend(page);
    }
    Ok(all)
}

/// Drop builds (and their child tasks) not matching the fetch
/// scope. Unattributed tasks are kept only under no filters —
/// with filters we can't prove they match.
fn apply_filters(incoming: &mut Dataset, opts: &FetchOpts) {
    if !opts.filtered() {
        return;
    }
    let keep_build = |b: &BuildRecord| -> bool {
        if let Some(owner) = &opts.owner
            && b.owner.as_deref() != Some(owner.as_str())
        {
            return false;
        }
        if let Some(packages) = &opts.packages {
            match &b.package {
                Some(p) => packages.contains(p),
                None => false,
            }
        } else {
            true
        }
    };
    incoming.builds.retain(|_, b| keep_build(b));
    let kept: BTreeSet<String> = incoming.builds.keys().cloned().collect();
    incoming.tasks.retain(|_, t| match t.parent {
        Some(parent) => kept.contains(&format!("{}:{parent}", t.instance)),
        None => false,
    });
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

fn build_record(instance: &str, task: &sandogasa_kojihub::HubTask) -> BuildRecord {
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
    }
}

/// Convert a buildArch task; `None` (with a warning) when the
/// record is unusable for lag analysis.
fn task_record(instance: &str, task: &sandogasa_kojihub::HubTask) -> Option<TaskRecord> {
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

    #[test]
    fn walk_progress_says_how_far_back_it_has_reached() {
        let day = 86_400.0;
        let now = 1_780_000_000.0;
        // A ten-day window, 1000 tasks per page.
        let mut progress = WalkProgress::new(now - 10.0 * day, 1000);

        // First page covers one day: 1000 tasks in a day means about
        // nine more days and nine more pages to go.
        let first = progress.note([now, now - day], 1000);
        assert!(
            first.contains("page 1 (1000 task(s), 1000 so far)"),
            "{first}"
        );
        assert!(first.contains("10% of the window"), "{first}");
        assert!(first.contains("~9 page(s) to go"), "{first}");

        // Five days in, the count and the estimate move together.
        let fifth = progress.note([now - 5.0 * day], 1000);
        assert!(
            fifth.contains("page 2 (1000 task(s), 2000 so far)"),
            "{fifth}"
        );
        assert!(fifth.contains("50% of the window"), "{fifth}");
        assert!(fifth.contains("~2 page(s) to go"), "{fifth}");

        // At the target there is nothing left to project.
        let last = progress.note([now - 10.0 * day], 40);
        assert!(last.contains("100% of the window"), "{last}");
        assert!(last.contains("~0 page(s) to go"), "{last}");
    }

    #[test]
    fn walk_progress_without_timestamps_still_counts_pages() {
        // Tasks the hub sent without a creation time say nothing about
        // position, so the line claims nothing about it.
        let mut progress = WalkProgress::new(0.0, 1000);
        let line = progress.note([], 7);
        assert_eq!(line, "page 1 (7 task(s))");
        assert!(!line.contains('%'));
    }
    use std::collections::HashMap;

    use super::*;

    fn value_str(s: &str) -> Value {
        Value::String(s.to_string())
    }

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }

    /// XML for one buildArch task struct.
    fn arch_task_xml(id: i64, parent: i64, arch: &str, srpm: &str, completion: f64) -> String {
        format!(
            "<value><struct>\
             <member><name>id</name><value><int>{id}</int></value></member>\
             <member><name>parent</name><value><int>{parent}</int></value></member>\
             <member><name>method</name><value><string>buildArch</string></value></member>\
             <member><name>arch</name><value><string>{arch}</string></value></member>\
             <member><name>state</name><value><int>2</int></value></member>\
             <member><name>create_ts</name><value><double>100.0</double></value></member>\
             <member><name>start_ts</name><value><double>160.0</double></value></member>\
             <member><name>completion_ts</name><value><double>{completion}</double></value></member>\
             <member><name>host_id</name><value><int>643</int></value></member>\
             <member><name>request</name><value><array><data>\
             <value><string>tasks/1/2/{srpm}</string></value>\
             <value><int>128157</int></value>\
             <value><string>{arch}</string></value>\
             </data></array></value></member>\
             </struct></value>"
        )
    }

    /// XML for one parent build task struct.
    fn build_task_xml(id: i64, owner: &str, scratch: bool, completion: f64) -> String {
        let scratch_member = if scratch {
            "<member><name>scratch</name><value><boolean>1</boolean></value></member>"
        } else {
            ""
        };
        format!(
            "<value><struct>\
             <member><name>id</name><value><int>{id}</int></value></member>\
             <member><name>method</name><value><string>build</string></value></member>\
             <member><name>state</name><value><int>2</int></value></member>\
             <member><name>create_ts</name><value><double>90.0</double></value></member>\
             <member><name>start_ts</name><value><double>95.0</double></value></member>\
             <member><name>completion_ts</name><value><double>{completion}</double></value></member>\
             <member><name>owner_name</name><value><string>{owner}</string></value></member>\
             <member><name>request</name><value><array><data>\
             <value><string>git+https://src.fedoraproject.org/rpms/foo.git#abc</string></value>\
             <value><string>f45-candidate</string></value>\
             <value><struct>{scratch_member}\
             <member><name>repo_id</name><value><int>1</int></value></member>\
             </struct></value>\
             </data></array></value></member>\
             </struct></value>"
        )
    }

    fn array_response(inner: &str) -> String {
        format!(
            "<?xml version='1.0'?><methodResponse><params><param>\
             <value><array><data>{inner}</data></array></value>\
             </param></params></methodResponse>"
        )
    }

    fn id_name_response(id: i64, name: &str) -> String {
        array_response(&format!(
            "<value><struct>\
             <member><name>id</name><value><int>{id}</int></value></member>\
             <member><name>name</name><value><string>{name}</string></value></member>\
             </struct></value>"
        ))
    }

    /// The full fetch flow against a mock hub: pagination, the
    /// parent join, package extraction (own srpm + inherited),
    /// scratch detection via an orphan parent fetch, host maps,
    /// and the recorded coverage window.
    #[test]
    fn fetch_end_to_end() {
        use wiremock::matchers::{body_string_contains, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = block_on(MockServer::start());

        block_on(
            Mock::given(method("POST"))
                .and(body_string_contains("<methodName>listHosts</methodName>"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_string(id_name_response(643, "buildvm-s390x-01.s390")),
                )
                .mount(&server),
        );
        block_on(
            Mock::given(method("POST"))
                .and(body_string_contains(
                    "<methodName>listChannels</methodName>",
                ))
                .respond_with(
                    ResponseTemplate::new(200).set_body_string(id_name_response(1, "default")),
                )
                .mount(&server),
        );

        // Build walk (newest-first by id): one official and one
        // scratch build in a single short page — walk mechanics
        // are covered in the hub crate's tests.
        let builds_page = array_response(&format!(
            "{}{}",
            build_task_xml(1, "alice", false, 600.0),
            build_task_xml(99, "bob", true, 250.0)
        ));
        block_on(
            Mock::given(method("POST"))
                .and(body_string_contains("<methodName>listTasks</methodName>"))
                .and(body_string_contains("-id"))
                .respond_with(ResponseTemplate::new(200).set_body_string(builds_page))
                .expect(1)
                .mount(&server),
        );
        // Children come back via the indexed parent-batch query.
        let children_page = array_response(&format!(
            "{}{}{}",
            arch_task_xml(11, 1, "x86_64", "foo-1.0-1.fc45.src.rpm", 200.0),
            arch_task_xml(12, 1, "s390x", "foo-1.0-1.fc45.src.rpm", 500.0),
            arch_task_xml(21, 99, "aarch64", "bar-2.0-1.fc45.src.rpm", 300.0)
        ));
        block_on(
            Mock::given(method("POST"))
                .and(body_string_contains("<methodName>listTasks</methodName>"))
                .and(body_string_contains("<name>parent</name>"))
                .respond_with(ResponseTemplate::new(200).set_body_string(children_page))
                .expect(1)
                .mount(&server),
        );

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("dataset.json");
        let opts = FetchOpts {
            instance_key: "fedora".to_string(),
            hub_url: server.uri(),
            after: 0.0,
            before: 1000.0,
            owner: None,
            packages: None,
            page_size: 4,
            sleep_ms: 0,
            retries: 0,
            verbose: false,
        };
        let report = run(&opts, &out).unwrap();
        assert_eq!(report.tasks_swept, 3);
        assert_eq!(report.builds_swept, 2);

        let ds = Dataset::load(&out).unwrap();
        assert_eq!(ds.tasks.len(), 3);
        assert_eq!(ds.builds.len(), 2);
        // Scratch came from the orphan-fetched parent.
        assert!(ds.builds["fedora:99"].scratch);
        assert!(!ds.builds["fedora:1"].scratch);
        // Package names: from the child's own srpm.
        assert_eq!(ds.tasks["fedora:11"].package.as_deref(), Some("foo"));
        assert_eq!(ds.tasks["fedora:21"].package.as_deref(), Some("bar"));
        // Host map is namespaced by instance.
        assert_eq!(
            ds.hosts.get("fedora:643").map(String::as_str),
            Some("buildvm-s390x-01.s390")
        );
        // Full-coverage window recorded.
        assert_eq!(ds.meta.windows.len(), 1);
        assert!(!ds.meta.windows[0].filtered);
        assert_eq!(ds.meta.windows[0].from, 0.0);
        assert_eq!(ds.meta.windows[0].to, 1000.0);

        // The report over this dataset attributes s390x.
        let out_report = crate::report::run(&ds, &crate::report::ReportOpts::default());
        assert_eq!(out_report.arches[0].arch, "s390x");
        assert_eq!(out_report.bottlenecked_builds, 1);
    }

    /// Filters drop non-matching builds and their children, and
    /// mark the window as filtered.
    #[test]
    fn filtered_fetch_drops_unmatched_and_marks_window() {
        let mut incoming = Dataset::new();
        let build = |id: i64, owner: &str, package: &str| BuildRecord {
            instance: "fedora".to_string(),
            task_id: id,
            package: Some(package.to_string()),
            nvr: None,
            target: None,
            owner: Some(owner.to_string()),
            scratch: false,
            state: 2,
            create_ts: 0.0,
            start_ts: None,
            completion_ts: None,
            priority: None,
        };
        let task = |id: i64, parent: i64| TaskRecord {
            instance: "fedora".to_string(),
            task_id: id,
            parent: Some(parent),
            method: "buildArch".to_string(),
            arch: "x86_64".to_string(),
            package: None,
            state: 2,
            create_ts: 0.0,
            start_ts: None,
            completion_ts: None,
            host_id: None,
            channel_id: None,
            weight: None,
        };
        incoming
            .builds
            .insert("fedora:1".into(), build(1, "alice", "foo"));
        incoming
            .builds
            .insert("fedora:2".into(), build(2, "bob", "bar"));
        incoming.tasks.insert("fedora:11".into(), task(11, 1));
        incoming.tasks.insert("fedora:21".into(), task(21, 2));
        // Unattributed task: dropped under filters.
        let mut orphan = task(31, 0);
        orphan.parent = None;
        incoming.tasks.insert("fedora:31".into(), orphan);

        let opts = FetchOpts {
            instance_key: "fedora".to_string(),
            hub_url: "https://unused.example".to_string(),
            after: 0.0,
            before: 1.0,
            owner: Some("alice".to_string()),
            packages: None,
            page_size: 1000,
            sleep_ms: 0,
            retries: 0,
            verbose: false,
        };
        apply_filters(&mut incoming, &opts);
        assert_eq!(incoming.builds.len(), 1);
        assert!(incoming.builds.contains_key("fedora:1"));
        assert_eq!(incoming.tasks.len(), 1);
        assert!(incoming.tasks.contains_key("fedora:11"));
        assert!(opts.filtered());
    }

    // ---- resolve_window ----

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
