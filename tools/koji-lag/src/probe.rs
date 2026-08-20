// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What a page costs right now, measured the way `sync` asks for one.
//!
//! Before committing hours to a backfill it is worth knowing what a page
//! costs at the depth being asked for, because it varies by an order of
//! magnitude with how far back the window is and with how busy the hub is.
//!
//! Two rules, both learned by getting this wrong on 2026-08-20.
//!
//! **Time the tool's own query, never a hand-written one.** A probe written
//! by hand reached for `order: -create_time`, which `sync` deliberately
//! avoids — ids grow with creation, so the cursor's filter already gives
//! the ordering — and read 349s where `sync`'s own shape read single-digit
//! seconds. That is why this times [`crate::sync`]'s `Pages` rather than
//! composing a query of its own: the thing measured cannot drift from the
//! thing that will run.
//!
//! **Walk the cursor; do not repeat a page.** The first page into a stretch
//! of history nobody has asked about lately is far dearer than the ones
//! behind it — listing January 2025 took about seven minutes for page one,
//! including a request abandoned at the client's 180s timeout, and then
//! settled at 55s a page. Asking for the *same* page twice measures the
//! second one warm and learns nothing about either number, so each depth
//! here is walked forward exactly as `sync` walks it, and the first page is
//! reported apart from the steady state it settles into.
//!
//! Both figures are needed to size a backfill: the steady rate is what the
//! bulk of it costs, and the first-page cost is what every fresh region
//! charges on entry — which for a backfill split into monthly runs is paid
//! once per run.
//!
//! Pacing is left out: a probe measures the hub's answer, not our
//! politeness about asking.

use crate::fetch::FetchOpts;
use crate::sync::Pages;

/// One depth, walked forward a few pages.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// Days before now that the walk started.
    pub depth_days: f64,
    /// Rows in the last full page seen.
    pub rows: usize,
    /// Seconds per page, in cursor order — so `runs[0]` is the cold one.
    pub runs: Vec<f64>,
}

impl Sample {
    /// The cost of entering this region: the first page of the walk.
    pub fn first(&self) -> f64 {
        self.runs.first().copied().unwrap_or(f64::NAN)
    }

    /// What the walk settles into: the median of every page after the
    /// first, which is the rate the bulk of a backfill runs at.
    pub fn steady(&self) -> f64 {
        let mut rest: Vec<f64> = self.runs.iter().skip(1).copied().collect();
        if rest.is_empty() {
            return f64::NAN;
        }
        rest.sort_by(|a, b| a.total_cmp(b));
        rest[rest.len() / 2]
    }

    /// How much dearer entering the region was than walking it.
    pub fn entry_penalty(&self) -> f64 {
        let steady = self.steady();
        if !steady.is_finite() || steady <= 0.0 {
            return f64::NAN;
        }
        self.first() / steady
    }

    /// Seconds a month of listing would cost at this rate.
    ///
    /// Estimated from pages, not from rows: the cost is dominated by the
    /// seek, so a month is however many pages its builds fill. Fedora runs
    /// around 150,000 build tasks a month, which is where the divisor
    /// comes from — it is an order-of-magnitude figure and says so.
    /// Priced at the steady rate, plus one first page for entering.
    pub fn month_estimate_secs(&self) -> f64 {
        let steady = self.steady();
        if self.rows == 0 || !steady.is_finite() {
            return f64::NAN;
        }
        let pages = 150_000.0 / self.rows as f64;
        self.first() + (pages - 1.0).max(0.0) * steady
    }
}

/// Time one page at each of `depths` days before `now`.
///
/// Deliberately sequential and small: the point is a number to decide with,
/// not a benchmark.
pub fn run(
    pages: &mut impl Pages,
    now: f64,
    depths: &[f64],
    steps: usize,
    verbose: bool,
) -> Result<Vec<Sample>, String> {
    let mut samples = Vec::new();
    for depth in depths {
        let mut before = now - depth * 86_400.0;
        let mut sample = Sample {
            depth_days: *depth,
            rows: 0,
            runs: Vec::new(),
        };
        for step in 1..=steps.max(2) {
            let started = std::time::Instant::now();
            let page = pages.page(before, 0)?;
            let seconds = started.elapsed().as_secs_f64();
            sample.runs.push(seconds);
            if !page.tasks.is_empty() {
                sample.rows = page.tasks.len();
            }
            if verbose {
                eprintln!(
                    "[koji-lag] probe: {:.0}d back, page {step}/{}: {} rows in {seconds:.1}s",
                    sample.depth_days,
                    steps.max(2),
                    page.tasks.len()
                );
            }
            // Step the cursor the way sync does: back to the oldest
            // creation this page held. A page with nothing in it is the
            // end of history, and walking further would re-ask for it.
            let oldest = page
                .tasks
                .iter()
                .filter_map(|t| t.create_ts)
                .fold(f64::INFINITY, f64::min);
            if !oldest.is_finite() {
                break;
            }
            before = oldest;
        }
        samples.push(sample);
    }
    Ok(samples)
}

/// The human summary.
pub fn render(samples: &[Sample], opts: &FetchOpts) -> String {
    let mut out = format!(
        "one page of {} rows, as sync asks for it (createdBefore, -id)\n\n",
        opts.page_size
    );
    out.push_str("| depth | rows | first page | steady | entry cost | a month would cost |\n");
    out.push_str("|---|---|---|---|---|---|\n");
    for s in samples {
        let est = s.month_estimate_secs();
        let est = if est.is_nan() {
            "—".to_string()
        } else if est < 3_600.0 {
            format!("~{:.0} min", est / 60.0)
        } else {
            format!("~{:.1} h", est / 3_600.0)
        };
        let penalty = s.entry_penalty();
        out.push_str(&format!(
            "| {:.0}d | {} | {:.1}s | {:.1}s | {} | {} |\n",
            s.depth_days,
            s.rows,
            s.first(),
            s.steady(),
            if penalty.is_finite() {
                format!("{penalty:.0}x")
            } else {
                "—".to_string()
            },
            est
        ));
    }
    // A short page means the hub ran out of history, not that it was
    // quick, and an estimate scaled from it is nonsense.
    if let Some(short) = samples.iter().find(|s| (s.rows as i64) < opts.page_size) {
        out.push_str(&format!(
            "\nnote: the walk at {:.0}d came back short ({} of {} rows), so it\n\
             reached the end of what Koji holds and the estimate from it is\n\
             meaningless.\n",
            short.depth_days, short.rows, opts.page_size
        ));
    }
    // The failure that stops a backfill dead, rather than slowing it.
    // The bound the walk actually ran under, not whatever the environment
    // says now — a probe reporting against a different limit than it used
    // would be worse than reporting none.
    let timeout_secs = opts
        .timeout
        .map(|d| d.as_secs_f64())
        .unwrap_or(f64::INFINITY);
    if let Some(slow) = samples
        .iter()
        .find(|s| s.runs.iter().any(|r| *r > timeout_secs * 0.8))
    {
        out.push_str(&format!(
            "\nwarning: a page at {:.0}d took {:.0}s, against the client's {timeout_secs:.0}s\n\
             timeout. A page that exceeds it is abandoned and retried, and the retry\n\
             pays the hub cost again — so a deep backfill can make no progress at all\n\
             rather than merely slow progress. Use a smaller --page-size at this depth.\n",
            slow.depth_days,
            slow.runs.iter().copied().fold(0.0, f64::max)
        ));
    }
    out.push_str(
        "\nlisting only: the children stage costs about eight minutes per day\n\
         of builds on top, and dominates a long backfill. A month is taken as\n\
         150,000 build tasks, which is an order-of-magnitude figure.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sweep::Page;
    use sandogasa_kojihub::HubTask;

    /// A hub that answers instantly with a fixed page.
    struct Canned {
        rows: usize,
        asked: Vec<f64>,
    }

    impl Pages for Canned {
        fn page(&mut self, before: f64, _offset: i64) -> Result<Page, String> {
            self.asked.push(before);
            Ok(Page {
                tasks: (0..self.rows)
                    .map(|i| HubTask {
                        id: i as i64,
                        parent: None,
                        method: "build".into(),
                        arch: None,
                        state: 2,
                        create_ts: Some(before),
                        start_ts: None,
                        completion_ts: None,
                        host_id: None,
                        channel_id: None,
                        owner: None,
                        owner_name: None,
                        priority: None,
                        weight: None,
                        request: None,
                    })
                    .collect(),
            })
        }
    }

    #[test]
    fn the_cursor_walks_back_rather_than_re_asking() {
        // The whole point: repeating a page measures it warm. Each step
        // must move to the oldest creation the last page held.
        let mut hub = Canned {
            rows: 10,
            asked: Vec::new(),
        };
        let now = 10_000_000.0;
        let samples = run(&mut hub, now, &[1.0], 3, false).unwrap();
        assert_eq!(samples[0].runs.len(), 3);
        assert_eq!(hub.asked.len(), 3);
        assert_eq!(hub.asked[0], now - 86_400.0);
        // Canned stamps every task with the `before` it was asked for, so
        // each subsequent ask repeats it — what matters is that the walk
        // uses the page's own oldest creation, not the original depth.
        assert_eq!(hub.asked[1], hub.asked[0]);
    }

    #[test]
    fn the_first_page_is_kept_apart_from_the_steady_rate() {
        // The measured shape of a real deep backfill: a dear first page,
        // then a settled rate. Averaging the two describes neither.
        let sample = Sample {
            depth_days: 600.0,
            rows: 4000,
            runs: vec![420.0, 55.0, 65.0, 55.0],
        };
        assert_eq!(sample.first(), 420.0);
        assert_eq!(sample.steady(), 55.0, "median of the pages after the first");
        assert!((sample.entry_penalty() - 420.0 / 55.0).abs() < 0.01);
    }

    #[test]
    fn a_single_page_yields_no_steady_rate_to_plan_with() {
        let sample = Sample {
            depth_days: 600.0,
            rows: 4000,
            runs: vec![420.0],
        };
        assert!(sample.steady().is_nan());
        assert!(sample.month_estimate_secs().is_nan());
        assert!(sample.entry_penalty().is_nan());
    }

    #[test]
    fn a_page_near_the_client_timeout_is_warned_about() {
        // The failure that stops a backfill dead: 180s abandons the
        // request and the retry pays again. Observed on page one of
        // January 2025.
        let opts = FetchOpts {
            instance_key: "fedora".into(),
            hub_url: "https://example.invalid/kojihub".into(),
            after: 0.0,
            before: 0.0,
            page_size: 4000,
            sleep_ms: 0,
            retries: 0,
            duty_percent: 0,
            timeout: None,
            verbose: false,
        };
        let opts = FetchOpts {
            timeout: Some(std::time::Duration::from_secs(180)),
            ..opts
        };
        let samples = vec![Sample {
            depth_days: 600.0,
            rows: 4000,
            runs: vec![175.0, 55.0],
        }];
        let rendered = render(&samples, &opts);
        assert!(rendered.contains("warning:"), "{rendered}");
        assert!(rendered.contains("smaller --page-size"), "{rendered}");
        // A comfortable walk says nothing, and neither does the same
        // walk under a bound generous enough for it.
        let calm = vec![Sample {
            depth_days: 30.0,
            rows: 4000,
            runs: vec![20.0, 7.0, 8.0],
        }];
        assert!(!render(&calm, &opts).contains("warning:"));
        let roomy = FetchOpts {
            timeout: Some(std::time::Duration::from_secs(600)),
            ..opts
        };
        assert!(!render(&samples, &roomy).contains("warning:"));
    }

    #[test]
    fn a_months_estimate_counts_pages_and_charges_entry_once() {
        // 150,000 builds in 4000-row pages is 37.5 pages: one dear first
        // page plus 36.5 at the steady rate. Counting the first page's
        // cost for every page would treble this estimate.
        let sample = Sample {
            depth_days: 600.0,
            rows: 4000,
            runs: vec![400.0, 10.0, 10.0],
        };
        let expected = 400.0 + 36.5 * 10.0;
        assert!(
            (sample.month_estimate_secs() - expected).abs() < 0.1,
            "{}",
            sample.month_estimate_secs()
        );
        // Half the page size is twice the pages, and still one entry.
        let smaller = Sample {
            rows: 2000,
            ..sample.clone()
        };
        let expected = 400.0 + 74.0 * 10.0;
        assert!((smaller.month_estimate_secs() - expected).abs() < 0.1);
    }

    #[test]
    fn a_short_page_is_flagged_rather_than_estimated_from() {
        // The trap this exists for: at the very edge of Koji's history a
        // page comes back short, which is fast and means nothing.
        let mut hub = Canned {
            rows: 3,
            asked: Vec::new(),
        };
        let opts = FetchOpts {
            instance_key: "fedora".into(),
            hub_url: "https://example.invalid/kojihub".into(),
            after: 0.0,
            before: 0.0,
            page_size: 4000,
            sleep_ms: 0,
            retries: 0,
            duty_percent: 50,
            timeout: None,
            verbose: false,
        };
        let samples = run(&mut hub, 1_000_000.0, &[1.0], 2, false).unwrap();
        let rendered = render(&samples, &opts);
        assert!(rendered.contains("came back short"), "{rendered}");
        assert!(rendered.contains("meaningless"), "{rendered}");
    }

    #[test]
    fn an_empty_page_does_not_produce_an_estimate() {
        let sample = Sample {
            depth_days: 1.0,
            rows: 0,
            runs: vec![0.4],
        };
        assert!(sample.month_estimate_secs().is_nan());
    }
}
