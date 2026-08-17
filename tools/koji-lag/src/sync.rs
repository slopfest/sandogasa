// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bringing the store up to date with the hub.
//!
//! A sync asks for two things and only what is missing of each: the build
//! tasks created over a span, and the child tasks of builds that have none.
//! Both are tracked in the store, so an interrupted run resumes where it
//! stopped and a re-run of a covered window costs nothing at all.
//!
//! The listing walks backwards by creation time with a cursor (see
//! [`crate::sweep`]), which is what lets a window months old be reached
//! without paging through everything since.

use sandogasa_kojihub::{HubClient, HubTask, ListTasksOpts, QueryOpts, retry};

use crate::dataset::{BuildRecord, TaskRecord};
use crate::fetch::{CREATE_GRACE_SECS, FetchOpts, PARENT_CHUNK, build_record, task_record};
use crate::store::{CHILDREN_GEN, Span, Store};
use crate::sweep::{Cursor, Page, step};

/// What a sync did, for the summary line.
#[derive(Debug, Default, PartialEq)]
pub struct SyncReport {
    /// Listing pages fetched.
    pub pages: usize,
    /// Build rows written.
    pub builds: usize,
    /// Child task rows written.
    pub tasks: usize,
    /// Builds whose children were fetched.
    pub parents_swept: usize,
    /// Creation spans that needed listing, and what they were.
    pub gaps: Vec<Span>,
    /// Seconds of the wanted span the store already covered.
    pub already_listed: f64,
}

/// A source of build-task pages, newest first.
///
/// A trait so the walk can be tested without a hub: the awkward parts are
/// the cursor arithmetic and when coverage may be claimed, and neither
/// needs a network to exercise.
pub trait Pages {
    /// Build tasks created before `before`, skipping `offset` of them.
    fn page(&mut self, before: f64, offset: i64) -> Result<Page, String>;
}

/// Pages from a Koji hub, paced by how long the hub takes to answer.
pub struct HubPages<'a> {
    hub: &'a HubClient,
    opts: &'a FetchOpts,
}

impl<'a> HubPages<'a> {
    pub fn new(hub: &'a HubClient, opts: &'a FetchOpts) -> Self {
        Self { hub, opts }
    }
}

impl Pages for HubPages<'_> {
    fn page(&mut self, before: f64, offset: i64) -> Result<Page, String> {
        let list_opts = ListTasksOpts {
            method: Some("build".to_string()),
            created_before: Some(before),
            decode: true,
            ..Default::default()
        };
        let query = QueryOpts {
            limit: Some(self.opts.page_size),
            offset: (offset > 0).then_some(offset),
            // By id descending, which for tasks is creation order: ids
            // only grow. Ordering by creation time would ask the hub to
            // sort a column the cursor is already filtering on.
            order: Some("-id".to_string()),
        };
        let started = std::time::Instant::now();
        let tasks = retry(self.opts.retries, || {
            self.hub.list_tasks(&list_opts, &query)
        })
        .map_err(|e| format!("listTasks(build, createdBefore) failed: {e}"))?;
        self.opts.pace().rest(started.elapsed());
        Ok(Page { tasks })
    }
}

/// Sync `[opts.after, opts.before)` into `store`.
pub fn run(store: &mut Store, opts: &FetchOpts) -> Result<SyncReport, String> {
    let hub = HubClient::new(&opts.hub_url);
    // Cheap first: an unreachable hub must fail in seconds. This doubles
    // as the host and channel refresh, which reports need to name arches.
    let hosts = retry(opts.retries, || hub.list_hosts_with_arches())
        .map_err(|e| format!("cannot reach the hub at {}: {e}", opts.hub_url))?;
    let channels =
        retry(opts.retries, || hub.list_channels()).map_err(|e| format!("listChannels: {e}"))?;
    store.put_hosts(&opts.instance_key, &hosts)?;
    store.put_channels(&opts.instance_key, &channels)?;

    // Creation span the completion window needs: back past its start by
    // more than the longest build, since a build created earlier can
    // finish inside it. Nothing newer than the window's end can belong to
    // it, so there is no margin on that side.
    let want = Span {
        from: opts.after - CREATE_GRACE_SECS,
        to: opts.before,
    };
    let gaps = store.gaps(&opts.instance_key, want)?;
    let missing: f64 = gaps.iter().map(|g| g.to - g.from).sum();
    let mut report = SyncReport {
        gaps: gaps.clone(),
        already_listed: (want.to - want.from - missing).max(0.0),
        ..Default::default()
    };
    if opts.verbose {
        say_the_plan(&want, &gaps, report.already_listed);
    }

    let mut pages = HubPages::new(&hub, opts);
    // Newest gap first, so an interrupted sync has left the recent end of
    // the window usable rather than a hole in the middle of it.
    for gap in gaps.iter().rev() {
        let filled = fill_gap(store, &opts.instance_key, *gap, &mut pages, opts)?;
        report.pages += filled.pages;
        report.builds += filled.builds;
    }

    // Children second, and for the whole window rather than for what this
    // run listed: a previous run may have listed builds and been
    // interrupted before their children, and there is nothing to be
    // gained by leaving those behind.
    let swept = sweep_children(store, &hub, opts)?;
    report.parents_swept = swept.parents;
    report.tasks = swept.tasks;
    Ok(report)
}

/// What filling one gap cost.
#[derive(Debug, Default)]
pub struct Filled {
    pub pages: usize,
    pub builds: usize,
}

/// List every build task created in `gap`, storing as it goes.
///
/// Coverage is recorded page by page, so an interrupted walk keeps credit
/// for exactly what it read: the next run subtracts those spans and
/// carries on from the edge rather than starting the gap again.
pub fn fill_gap(
    store: &mut Store,
    instance: &str,
    gap: Span,
    pages: &mut impl Pages,
    opts: &FetchOpts,
) -> Result<Filled, String> {
    let page_size = opts.page_size.max(1) as usize;
    let mut cursor = Cursor {
        before: gap.to,
        offset: 0,
    };
    let mut filled = Filled::default();
    loop {
        let page = pages.page(cursor.before, cursor.offset)?;
        filled.pages += 1;
        let builds: Vec<BuildRecord> = page
            .tasks
            .iter()
            .map(|t| build_record(instance, t))
            .collect();
        filled.builds += store.put_builds(instance, &builds)?;

        let outcome = step(&page, cursor, gap, page_size);
        // The rows before the claim: a claim is a promise that everything
        // created in the span is in the store, so it must not be written
        // before the rows it vouches for.
        if let Some(span) = outcome.listed {
            store.add_listed(instance, span)?;
        }
        if opts.verbose {
            eprintln!(
                "[koji-lag] sync: {}",
                progress(&filled, &page, &outcome, gap)
            );
        }
        match outcome.next {
            Some(next) => cursor = next,
            None => break,
        }
    }
    Ok(filled)
}

/// What a page's worth of walking has achieved, in terms of the gap.
///
/// The position is exact rather than estimated: the gap has known bounds,
/// so how much of it is left is arithmetic, and the pages so far give the
/// rate. What the previous design could only guess at from task density
/// (and revised on every page) is now just a subtraction.
fn progress(filled: &Filled, page: &Page, outcome: &crate::sweep::Step, gap: Span) -> String {
    let reached = outcome
        .listed
        .map(|s| s.from)
        .unwrap_or(gap.to)
        .max(gap.from);
    let day = |ts: f64| {
        chrono::DateTime::from_timestamp(ts as i64, 0)
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "?".to_string())
    };
    let whole = (gap.to - gap.from).max(1.0);
    let done = ((gap.to - reached) / whole * 100.0).clamp(0.0, 100.0);
    let mut line = format!(
        "page {} ({} task(s)), listed back to {} — {done:.0}% of the gap",
        filled.pages,
        page.tasks.len(),
        day(reached),
    );
    if outcome.next.is_none() {
        line += ", done";
        return line;
    }
    // Rate over what this walk has covered, projected over what is left.
    let covered = gap.to - reached;
    if covered > 0.0 {
        let left = (reached - gap.from).max(0.0);
        let pages_left = (filled.pages as f64 * left / covered).ceil() as usize;
        line += &format!(", ~{pages_left} page(s) to go");
    }
    line
}

/// What a children sweep did.
#[derive(Debug, Default)]
pub struct Swept {
    pub parents: usize,
    pub tasks: usize,
}

/// Fetch the children of every build in the window that lacks them.
///
/// Each accepted batch is stored and marked before the next is asked for,
/// so an interrupted sweep never re-asks for children it already has —
/// which matters more here than in the listing, since this is the
/// expensive half: a day of Fedora is around 200 of these queries.
pub fn sweep_children(
    store: &mut Store,
    hub: &HubClient,
    opts: &FetchOpts,
) -> Result<Swept, String> {
    let pending = store.builds_needing_children(&opts.instance_key, opts.after, opts.before)?;
    let mut swept = Swept::default();
    if pending.is_empty() {
        if opts.verbose {
            eprintln!("[koji-lag] sync: every build in the window has its children");
        }
        return Ok(swept);
    }
    if opts.verbose {
        eprintln!(
            "[koji-lag] sync: children of {} build(s), {} at a time",
            pending.len(),
            PARENT_CHUNK
        );
    }
    let mut store_batch = |parents: &[i64], tasks: Vec<HubTask>| -> Result<(), String> {
        let records: Vec<TaskRecord> = tasks
            .iter()
            .filter_map(|t| task_record(&opts.instance_key, t))
            .collect();
        swept.tasks += store.put_tasks(&opts.instance_key, &records)?;
        // Marked whether or not anything came back: a build that failed
        // before it started an arch task has no children, and asking
        // again every run would never learn otherwise.
        store.mark_children_swept(&opts.instance_key, parents, CHILDREN_GEN)?;
        swept.parents += parents.len();
        Ok(())
    };
    crate::fetch::fetch_children_batched(hub, &pending, opts, &mut store_batch)?;
    Ok(swept)
}

fn say_the_plan(want: &Span, gaps: &[Span], already: f64) {
    let day = |ts: f64| {
        chrono::DateTime::from_timestamp(ts as i64, 0)
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "?".to_string())
    };
    let days = |secs: f64| secs / 86_400.0;
    eprintln!(
        "[koji-lag] sync: builds created {} to {} ({:.1} day(s)) — the window plus {} day(s) \
         before it, for builds that started earlier and finish inside it",
        day(want.from),
        day(want.to),
        days(want.to - want.from),
        (CREATE_GRACE_SECS / 86_400.0) as i64,
    );
    if gaps.is_empty() {
        eprintln!("[koji-lag] sync: already listed in full; nothing to fetch");
        return;
    }
    eprintln!(
        "[koji-lag] sync: {} gap(s) to list, {:.1} day(s) of it; {:.1} day(s) already listed",
        gaps.len(),
        days(gaps.iter().map(|g| g.to - g.from).sum()),
        days(already),
    );
    for gap in gaps.iter().rev() {
        eprintln!(
            "[koji-lag]   gap {} to {} ({:.1} day(s))",
            day(gap.from),
            day(gap.to),
            days(gap.to - gap.from)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hub that serves tasks out of a fixed list, honouring the cursor
    /// the same way `listTasks` does: newest first, created strictly
    /// before the bound, skipping `offset`.
    struct Canned {
        creations: Vec<f64>,
        page_size: usize,
        pub asked: Vec<(f64, i64)>,
    }

    impl Canned {
        fn new(creations: &[f64], page_size: usize) -> Self {
            let mut creations = creations.to_vec();
            creations.sort_by(|a, b| b.partial_cmp(a).unwrap());
            Self {
                creations,
                page_size,
                asked: Vec::new(),
            }
        }
    }

    impl Pages for Canned {
        fn page(&mut self, before: f64, offset: i64) -> Result<Page, String> {
            self.asked.push((before, offset));
            let tasks = self
                .creations
                .iter()
                .enumerate()
                .filter(|(_, ts)| **ts < before)
                .skip(offset as usize)
                .take(self.page_size)
                // The id descends with creation time, as Koji's do.
                .map(|(i, ts)| task(100_000 - i as i64, *ts))
                .collect();
            Ok(Page { tasks })
        }
    }

    fn task(id: i64, create_ts: f64) -> HubTask {
        HubTask {
            id,
            parent: None,
            method: "build".into(),
            arch: None,
            state: 2,
            create_ts: Some(create_ts),
            start_ts: Some(create_ts + 1.0),
            completion_ts: Some(create_ts + 2.0),
            host_id: None,
            channel_id: None,
            owner: None,
            owner_name: None,
            priority: None,
            weight: None,
            request: None,
        }
    }

    fn opts(page_size: i64) -> FetchOpts {
        FetchOpts {
            instance_key: "fedora".into(),
            hub_url: "https://example.invalid/kojihub".into(),
            after: 0.0,
            before: 10_000.0,
            page_size,
            sleep_ms: 0,
            retries: 1,
            duty_percent: 100,
            verbose: false,
        }
    }

    const GAP: Span = Span {
        from: 1_000.0,
        to: 10_000.0,
    };

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

    /// The whole flow against a mock hub: the hosts and channels probe,
    /// one listing page, the children by parent batch, and what all of it
    /// leaves in the store — including the coverage claim, without which a
    /// second sync would fetch the lot again.
    #[test]
    fn sync_end_to_end() {
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
        // One short page of build tasks: short because fewer rows came
        // back than were asked for, which is how the walk learns there is
        // nothing older and claims the gap to its far end.
        let builds_page = array_response(&format!(
            "{}{}",
            build_task_xml(1, "alice", false, 600.0),
            build_task_xml(99, "bob", true, 250.0)
        ));
        block_on(
            Mock::given(method("POST"))
                .and(body_string_contains("<methodName>listTasks</methodName>"))
                .and(body_string_contains("createdBefore"))
                .respond_with(ResponseTemplate::new(200).set_body_string(builds_page))
                .expect(1)
                .mount(&server),
        );
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

        let mut store = Store::in_memory().unwrap();
        let opts = FetchOpts {
            hub_url: server.uri(),
            page_size: 4,
            ..opts(4)
        };
        let report = run(&mut store, &opts).unwrap();
        assert_eq!(report.builds, 2);
        assert_eq!(report.tasks, 3);
        assert_eq!(report.parents_swept, 2);

        // What a report will see. Children come by parent, so the s390x
        // task finishing at 500 belongs to build 1 whatever the window.
        let ds = store
            .dataset_for("fedora", 0.0, 1000.0, crate::fetch::CREATE_GRACE_SECS)
            .unwrap();
        assert_eq!(ds.builds.len(), 2);
        assert_eq!(ds.tasks.len(), 3);
        assert!(ds.builds["fedora:99"].scratch, "scratch from the request");
        assert!(!ds.builds["fedora:1"].scratch);
        assert_eq!(ds.tasks["fedora:11"].package.as_deref(), Some("foo"));
        assert_eq!(ds.tasks["fedora:21"].package.as_deref(), Some("bar"));
        assert_eq!(
            ds.hosts.get("fedora:643").map(String::as_str),
            Some("buildvm-s390x-01.s390")
        );

        // The window and its three-day margin are claimed, so a second
        // sync of the same window asks the hub for no pages at all — the
        // mocks above would fail on a second listing request.
        assert!(
            store
                .gaps(
                    "fedora",
                    Span {
                        from: -crate::fetch::CREATE_GRACE_SECS,
                        to: 1000.0
                    }
                )
                .unwrap()
                .is_empty()
        );
        let again = run(&mut store, &opts).unwrap();
        assert_eq!(again.pages, 0, "nothing left to list");
        assert_eq!(again.parents_swept, 0, "nor any children to fetch");

        // And the report attributes the bottleneck to s390x, which is the
        // whole point of collecting any of it.
        let out = crate::report::run(&ds, &crate::report::ReportOpts::default());
        assert_eq!(out.arches[0].arch, "s390x");
        assert_eq!(out.bottlenecked_builds, 1);
    }

    #[test]
    fn filling_a_gap_stores_every_build_and_claims_it_whole() {
        let mut store = Store::in_memory().unwrap();
        let creations: Vec<f64> = (0..25).map(|i| 1_000.0 + i as f64 * 300.0).collect();
        let mut pages = Canned::new(&creations, 10);
        let filled = fill_gap(&mut store, "fedora", GAP, &mut pages, &opts(10)).unwrap();

        // Every creation inside the gap, once each.
        assert_eq!(store.counts().unwrap()["fedora"].builds, 25);
        assert!(filled.pages >= 3, "{filled:?}");
        // And the gap is closed, so a second sync asks for nothing.
        assert!(store.gaps("fedora", GAP).unwrap().is_empty());
    }

    #[test]
    fn a_second_sync_of_a_covered_gap_asks_for_nothing() {
        let mut store = Store::in_memory().unwrap();
        let creations: Vec<f64> = (0..25).map(|i| 1_000.0 + i as f64 * 300.0).collect();
        let mut pages = Canned::new(&creations, 10);
        fill_gap(&mut store, "fedora", GAP, &mut pages, &opts(10)).unwrap();

        // The gap is gone, so there is nothing left to fill: the point of
        // the store is that this costs no requests at all.
        let gaps = store.gaps("fedora", GAP).unwrap();
        assert!(gaps.is_empty());
        let before = pages.asked.len();
        for gap in gaps {
            fill_gap(&mut store, "fedora", gap, &mut pages, &opts(10)).unwrap();
        }
        assert_eq!(pages.asked.len(), before, "no further requests");
    }

    #[test]
    fn an_interrupted_walk_resumes_from_the_edge_it_reached() {
        let creations: Vec<f64> = (0..30).map(|i| 1_000.0 + i as f64 * 300.0).collect();
        let mut store = Store::in_memory().unwrap();

        // Walk one page by hand, as an interrupted sync would have.
        let mut pages = Canned::new(&creations, 10);
        let page = pages.page(GAP.to, 0).unwrap();
        let builds: Vec<BuildRecord> = page
            .tasks
            .iter()
            .map(|t| build_record("fedora", t))
            .collect();
        store.put_builds("fedora", &builds).unwrap();
        let outcome = step(
            &page,
            Cursor {
                before: GAP.to,
                offset: 0,
            },
            GAP,
            10,
        );
        store.add_listed("fedora", outcome.listed.unwrap()).unwrap();

        // What is left is a gap ending where that page stopped, so the
        // resumed walk never re-reads the newest ten.
        let left = store.gaps("fedora", GAP).unwrap();
        assert_eq!(left.len(), 1);
        assert!(left[0].to < GAP.to, "{left:?}");
        let mut fresh = Canned::new(&creations, 10);
        fill_gap(&mut store, "fedora", left[0], &mut fresh, &opts(10)).unwrap();
        assert!(
            fresh.asked.iter().all(|(before, _)| *before <= left[0].to),
            "resumed above the gap: {:?}",
            fresh.asked
        );
        // And the two halves together hold every build.
        assert_eq!(store.counts().unwrap()["fedora"].builds, 30);
        assert!(store.gaps("fedora", GAP).unwrap().is_empty());
    }

    #[test]
    fn a_crowded_second_is_drained_rather_than_stepped_over() {
        // 25 builds created in the same second — a mass rebuild's
        // submission — with a page that holds only 10 of them. The cursor
        // cannot move, so the walk must drain by offset or lose 15.
        let mut store = Store::in_memory().unwrap();
        let mut creations = vec![5_000.0; 25];
        creations.push(1_100.0);
        let mut pages = Canned::new(&creations, 10);
        fill_gap(&mut store, "fedora", GAP, &mut pages, &opts(10)).unwrap();
        assert_eq!(store.counts().unwrap()["fedora"].builds, 26);
        assert!(store.gaps("fedora", GAP).unwrap().is_empty());
    }
}
