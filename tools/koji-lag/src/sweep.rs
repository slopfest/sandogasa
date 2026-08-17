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

/// How far above a page's oldest creation the next cursor is placed.
///
/// It absorbs two things. *Ties*: many tasks can share a creation instant,
/// so a page can end in the middle of one and stepping to exactly that
/// instant would skip the rest of it. *Skew*: pages come back ordered by
/// task id, which is creation order only because Koji assigns both at
/// once — a row committed a moment late could carry a creation time above
/// a lower id's, and a cursor placed exactly at the boundary would step
/// over it.
///
/// The cost is re-reading this many seconds of creations per page, which
/// at Fedora's rate is well under a row on average, and the store dedupes
/// by task id. The benefit is that neither assumption has to hold exactly.
const REREAD_SECS: f64 = 5.0;

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
/// *Ties.* A page can end in the middle of a crowded moment, and moving
/// the cursor to exactly the page's oldest creation would skip the rest
/// of it. So the cursor moves [`REREAD_SECS`] *past* that creation and
/// the overlap is re-read; the store dedupes by task id, so the cost is a
/// few duplicated rows rather than missing ones. When a whole page spans
/// less than that margin the cursor cannot usefully move at all — the
/// next page would return the same rows — so the crowd is drained by
/// offset until a page spans more than the margin.
///
/// *What may be claimed.* A full page holds the newest rows below the
/// cursor, so everything created between its oldest row and the cursor
/// has been seen — except possibly more rows sharing that oldest moment.
/// The claim therefore starts a margin later. Under-claiming costs a
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
    let edge = oldest + REREAD_SECS;
    let listed = (cursor.before > edge).then_some(Span {
        from: edge.max(gap.from),
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
    let next = if newest - oldest < REREAD_SECS {
        // The page spans less than the margin it would be re-read by — a
        // mass rebuild submitting hundreds of builds at once does this —
        // so the next page would return these same rows and the walk
        // would never move. Such a crowd is drained by offset instead,
        // which is sound because offset pages come off one descending
        // list: everything above the deepest row has been seen.
        Cursor {
            before: cursor.before,
            offset: cursor.offset + page.tasks.len() as i64,
        }
    } else {
        Cursor {
            before: edge,
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
    fn the_cursor_steps_past_the_oldest_creation_it_saw() {
        let cursor = Cursor {
            before: 10_000.0,
            offset: 0,
        };
        let step = step(&page(&[9_900.0, 9_500.0, 9_000.0]), cursor, GAP, 3);
        // A full page, so the walk continues — from the margin past 9000,
        // not from 9000 itself.
        assert_eq!(
            step.next,
            Some(Cursor {
                before: 9_000.0 + REREAD_SECS,
                offset: 0
            })
        );
        // And claims only down to there: more rows may share that moment.
        assert_eq!(
            step.listed,
            Some(Span {
                from: 9_000.0 + REREAD_SECS,
                to: 10_000.0
            })
        );
    }

    #[test]
    fn a_page_inside_one_crowded_moment_is_drained_by_offset() {
        // What a mass rebuild does: hundreds of tasks created at once. The
        // cursor cannot move below them — the next page would return the
        // same rows, for ever — so the offset advances instead.
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
        // The claim still stands: everything between the margin above the
        // crowd and the cursor has been seen.
        assert_eq!(
            step.listed,
            Some(Span {
                from: 5_000.0 + REREAD_SECS,
                to: 10_000.0
            })
        );

        // Draining continues from where it left off.
        let again = super::step(&crowded, step.next.unwrap(), GAP, 4);
        assert_eq!(again.next.unwrap().offset, 8);
    }

    #[test]
    fn a_page_spanning_a_few_seconds_is_also_drained() {
        // Not literally one instant: a burst spread over a second or two
        // still cannot be stepped past, because the margin the next page
        // would re-read covers the whole burst.
        let cursor = Cursor {
            before: 10_000.0,
            offset: 0,
        };
        let burst = page(&[5_002.0, 5_001.0, 5_000.5, 5_000.0]);
        let step = step(&burst, cursor, GAP, 4);
        assert_eq!(step.next.unwrap().offset, 4, "must drain, not step");
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
