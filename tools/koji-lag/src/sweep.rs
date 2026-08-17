// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Listing build tasks by creation time, oldest-ward, into the store.
//!
//! `listTasks` accepts `createdBefore`, and that is the whole positioning
//! mechanism: each page asks for the newest tasks created before a
//! cursor, and the cursor moves back to where the page ended. Measured
//! against Fedora's hub, such a page costs 7–10s wherever it lands and
//! does not degrade with depth — so a window in April is reached as
//! directly as one from yesterday.
//!
//! What this replaces walked from the newest task by page offset, which
//! could only reach far history by paging through everything nearer. It
//! capped out at offset 500,000 — six weeks — and then spent minutes
//! re-listing data it already had. See DEVELOPMENT.md.

use crate::store::Span;

/// One page of build tasks, newest first.
pub struct Page {
    pub tasks: Vec<sandogasa_kojihub::HubTask>,
}

impl Page {
    fn creations(&self) -> impl Iterator<Item = f64> + '_ {
        self.tasks.iter().filter_map(|t| t.create_ts)
    }

    fn oldest(&self) -> Option<f64> {
        self.creations().reduce(f64::min)
    }

    fn newest(&self) -> Option<f64> {
        self.creations().reduce(f64::max)
    }
}

/// Where the next page should be asked from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cursor {
    /// Exclusive upper bound on creation time.
    pub before: f64,
    /// Rows to skip at that bound, used only to drain a crowded second.
    pub offset: i64,
}

/// What one page taught us: how far to claim, and where to go next.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Step {
    /// The creation span this page enumerated in full, if any.
    pub listed: Option<Span>,
    /// The next cursor, or `None` when the gap is covered.
    pub next: Option<Cursor>,
}

/// Read one page's worth of consequences.
///
/// Two things need care.
///
/// *Ties.* Many tasks can share a creation second — a mass rebuild
/// submits hundreds at once — so a page can end in the middle of one.
/// Moving the cursor to exactly that second would skip whatever else was
/// created in it. So the cursor moves to one second *past* the page's
/// oldest creation, which re-reads that second on the next page; the
/// store dedupes by task id, so the cost is a few duplicated rows rather
/// than missing ones. When an entire page shares a single second the
/// cursor cannot move at all, and the second is drained by offset
/// instead.
///
/// *What may be claimed.* A full page holds the newest rows below the
/// cursor, so everything created between its oldest row and the cursor
/// has been seen — except possibly the rest of that oldest second. The
/// claim therefore starts one second later. Under-claiming costs a
/// re-read; over-claiming loses builds.
pub fn step(page: &Page, cursor: Cursor, gap: Span, page_size: usize) -> Step {
    let (Some(oldest), Some(newest)) = (page.oldest(), page.newest()) else {
        // A page with no usable timestamps says nothing about coverage,
        // and nothing about where to go next either.
        return Step {
            listed: None,
            next: None,
        };
    };
    let listed = (cursor.before > oldest + 1.0).then_some(Span {
        from: (oldest + 1.0).max(gap.from),
        to: cursor.before,
    });

    // Reached the far side of the gap, or the hub ran out of tasks below
    // the cursor. Either way the gap is enumerated to its far end, and it
    // must be claimed to exactly there: claiming the usual second later
    // would leave a hole that never closes, so every later run would fetch
    // one more page of the same data for ever.
    if oldest <= gap.from || page.tasks.len() < page_size {
        return Step {
            listed: Some(Span {
                from: gap.from,
                to: cursor.before,
            }),
            next: None,
        };
    }
    let next = if oldest == newest {
        // The whole page is one second; the cursor would not move.
        Cursor {
            before: cursor.before,
            offset: cursor.offset + page.tasks.len() as i64,
        }
    } else {
        Cursor {
            before: oldest + 1.0,
            offset: 0,
        }
    };
    Step {
        listed,
        next: Some(next),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: i64, create_ts: f64) -> sandogasa_kojihub::HubTask {
        sandogasa_kojihub::HubTask {
            id,
            parent: None,
            method: "build".into(),
            arch: None,
            state: 2,
            create_ts: Some(create_ts),
            start_ts: None,
            completion_ts: None,
            host_id: None,
            channel_id: None,
            owner: None,
            owner_name: None,
            priority: None,
            weight: None,
            request: None,
        }
    }

    fn page(creations: &[f64]) -> Page {
        Page {
            tasks: creations
                .iter()
                .enumerate()
                .map(|(i, ts)| task(1000 - i as i64, *ts))
                .collect(),
        }
    }

    const GAP: Span = Span {
        from: 0.0,
        to: 10_000.0,
    };

    #[test]
    fn the_cursor_steps_past_the_oldest_second_it_saw() {
        let cursor = Cursor {
            before: 10_000.0,
            offset: 0,
        };
        let step = step(&page(&[9_900.0, 9_500.0, 9_000.0]), cursor, GAP, 3);
        // A short page (3 of 3 asked for is full, so not short) — this one
        // is full, so the walk continues from one second past 9000.
        assert_eq!(
            step.next,
            Some(Cursor {
                before: 9_001.0,
                offset: 0
            })
        );
        // And claims only down to that second, not into it: the rest of
        // second 9000 may not have been seen.
        assert_eq!(
            step.listed,
            Some(Span {
                from: 9_001.0,
                to: 10_000.0
            })
        );
    }

    #[test]
    fn a_page_that_is_all_one_second_is_drained_by_offset() {
        // What a mass rebuild does: hundreds of tasks created in the same
        // second. Moving the cursor to that second would skip the rest of
        // it, so the offset advances instead.
        let cursor = Cursor {
            before: 10_000.0,
            offset: 0,
        };
        let crowded = page(&[5_000.0; 4]);
        let step = step(&crowded, cursor, GAP, 4);
        assert_eq!(
            step.next,
            Some(Cursor {
                before: 10_000.0,
                offset: 4
            })
        );
        // Nothing older than the cursor is claimed while the second is
        // still being drained beyond what this page showed.
        assert_eq!(
            step.listed,
            Some(Span {
                from: 5_001.0,
                to: 10_000.0
            })
        );

        // Draining continues from where it left off.
        let again = super::step(&crowded, step.next.unwrap(), GAP, 4);
        assert_eq!(again.next.unwrap().offset, 8);
    }

    #[test]
    fn reaching_the_gap_ends_the_walk_and_claims_it_whole() {
        let cursor = Cursor {
            before: 10_000.0,
            offset: 0,
        };
        // This page crosses the gap's lower bound.
        let step = step(&page(&[9_000.0, 5_000.0, 0.0]), cursor, GAP, 3);
        assert_eq!(step.next, None);
        // Down to the gap's own edge, not a second inside it: a claim
        // that stops short leaves a hole no later sweep can close, and
        // every run would re-fetch a page to chase it.
        assert_eq!(step.listed, Some(GAP));
    }

    #[test]
    fn a_short_page_means_the_hub_has_no_more() {
        let cursor = Cursor {
            before: 10_000.0,
            offset: 0,
        };
        // Fewer rows than asked for: there is nothing older to fetch, so
        // the gap is covered to its far end even though no row reached it.
        let step = step(&page(&[9_000.0, 8_000.0]), cursor, GAP, 1000);
        assert_eq!(step.next, None);
        // Nothing older exists, so the gap is covered to its far end.
        assert_eq!(step.listed, Some(GAP));
    }

    #[test]
    fn a_page_without_timestamps_claims_nothing() {
        let cursor = Cursor {
            before: 10_000.0,
            offset: 0,
        };
        let mut undated = page(&[9_000.0]);
        undated.tasks[0].create_ts = None;
        let step = step(&undated, cursor, GAP, 1000);
        assert_eq!(step.listed, None);
        assert_eq!(step.next, None);
    }

    #[test]
    fn a_claim_never_reaches_past_the_gap_it_was_asked_for() {
        // The gap starts late; a page reaching further back must not
        // claim creation time nobody asked about, since the sweep did not
        // exhaust it.
        let gap = Span {
            from: 9_000.0,
            to: 10_000.0,
        };
        let cursor = Cursor {
            before: 10_000.0,
            offset: 0,
        };
        let step = step(&page(&[9_500.0, 1_000.0]), cursor, gap, 1000);
        assert_eq!(step.next, None);
        let claimed = step.listed.unwrap();
        assert!(claimed.from >= gap.from, "{claimed:?}");
    }
}
