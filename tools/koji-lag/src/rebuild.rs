// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Finding when a mass rebuild actually happened.
//!
//! The schedule says when one was meant to run (see [`crate::schedule`]),
//! and the two differ enough to be worth reporting side by side: F45's was
//! announced over four weeks and submitted in four days, F44's began two
//! days after its announced start, F43's exactly on it. The schedule's end
//! date is the branch date rather than the rebuild's, so it says almost
//! nothing about how long the work took.
//!
//! What the store can say instead is who submitted, day by day, and that
//! turns out to separate cleanly. Across 267 days measured in August 2026,
//! `releng`'s share of a day's builds was under 1% on 254 of them and above
//! 25% on eleven — the eleven being the three rebuilds. Two days lay
//! anywhere in between. So the threshold needs no tuning; anything from a
//! few percent to a quarter finds the same days.

use crate::store::Store;

/// A day the store holds, and how much of it was releng's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Day {
    /// UTC midnight.
    pub at: f64,
    pub builds: usize,
    pub releng: usize,
}

impl Day {
    pub fn share(&self) -> f64 {
        match self.builds {
            0 => 0.0,
            n => self.releng as f64 / n as f64,
        }
    }
}

/// A stretch of days when releng was submitting in bulk.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// First day, UTC midnight.
    pub from: f64,
    /// Exclusive end, so `to - from` is the length.
    pub to: f64,
    pub builds: usize,
    pub releng: usize,
}

impl Window {
    pub fn days(&self) -> usize {
        ((self.to - self.from) / 86_400.0).round() as usize
    }

    pub fn share(&self) -> f64 {
        match self.builds {
            0 => 0.0,
            n => self.releng as f64 / n as f64,
        }
    }
}

/// How to decide a day belongs to a rebuild.
#[derive(Debug, Clone, Copy)]
pub struct Rule {
    /// Minimum share of a day's builds submitted by releng.
    pub share: f64,
    /// Minimum consecutive days, so a small targeted rebuild of a handful
    /// of packages is not mistaken for a mass one.
    pub run: usize,
    /// Days with fewer builds than this are ignored rather than judged: a
    /// day at the edge of what has been collected holds an arbitrary
    /// fraction of itself, and a share computed over a hundred builds says
    /// nothing.
    pub floor: usize,
}

impl Default for Rule {
    fn default() -> Self {
        // A tenth sits in the middle of the empty range between the two
        // populations (under 1% against over 25%), so it is insensitive to
        // where exactly the line is drawn. Two days because every observed
        // rebuild ran three or four, while a targeted rebuild is one.
        Self {
            share: 0.10,
            run: 2,
            floor: 500,
        }
    }
}

/// releng's share of each day in `[from, to)`, oldest first.
pub fn days(store: &Store, instance: &str, from: f64, to: f64) -> Result<Vec<Day>, String> {
    store.releng_share_by_day(instance, from, to)
}

/// The stretches in `[from, to)` that look like a mass rebuild.
///
/// A day below `rule.floor` builds neither joins a run nor breaks one: it is
/// treated as absent, so a quiet Sunday inside a rebuild does not split it
/// in two while a half-collected day at the edge cannot invent one.
pub fn windows(days: &[Day], rule: Rule) -> Vec<Window> {
    let mut found: Vec<Window> = Vec::new();
    let mut current: Option<Window> = None;
    for day in days {
        if day.builds < rule.floor {
            continue;
        }
        let hot = day.share() >= rule.share;
        match (&mut current, hot) {
            (Some(w), true) => {
                w.to = day.at + 86_400.0;
                w.builds += day.builds;
                w.releng += day.releng;
            }
            (None, true) => {
                current = Some(Window {
                    from: day.at,
                    to: day.at + 86_400.0,
                    builds: day.builds,
                    releng: day.releng,
                });
            }
            (Some(_), false) => {
                if let Some(w) = current.take()
                    && w.days() >= rule.run
                {
                    found.push(w);
                }
            }
            (None, false) => {}
        }
    }
    if let Some(w) = current
        && w.days() >= rule.run
    {
        found.push(w);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(n: i64, builds: usize, releng: usize) -> Day {
        Day {
            at: n as f64 * 86_400.0,
            builds,
            releng,
        }
    }

    #[test]
    fn a_burst_of_releng_days_is_one_window() {
        // The shape every observed rebuild has: a ramp day, two or three
        // overwhelming ones, then back to nothing.
        let days = [
            day(1, 7000, 20),
            day(2, 7147, 1850),
            day(3, 6969, 6064),
            day(4, 11043, 10478),
            day(5, 8165, 6028),
            day(6, 7000, 15),
        ];
        let found = windows(&days, Rule::default());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].days(), 4);
        assert_eq!(found[0].from, 2.0 * 86_400.0);
        assert!(found[0].share() > 0.7, "{:?}", found[0].share());
    }

    #[test]
    fn one_busy_releng_day_is_not_a_mass_rebuild() {
        // A targeted rebuild of a few packages, or a day of releng tagging
        // work, should not be reported as a mass rebuild.
        let days = [day(1, 7000, 20), day(2, 6000, 5000), day(3, 7000, 30)];
        assert!(windows(&days, Rule::default()).is_empty());
    }

    #[test]
    fn a_thin_day_neither_starts_a_window_nor_splits_one() {
        // The middle day holds 40 builds — a fragment at the edge of what
        // was collected. Judging it would either invent a window or break a
        // real one in half; both have been seen in real data.
        let days = [
            day(1, 6000, 5500),
            day(2, 40, 0),
            day(3, 6000, 5400),
            day(4, 7000, 10),
        ];
        let found = windows(&days, Rule::default());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].days(), 3, "the thin day is spanned, not counted");

        // And a thin day cannot make a window on its own.
        let thin = [day(1, 10, 10), day(2, 12, 12), day(3, 8, 8)];
        assert!(windows(&thin, Rule::default()).is_empty());
    }

    #[test]
    fn the_threshold_is_insensitive_where_it_matters() {
        // The point of the calibration: the two populations are far enough
        // apart that any threshold between them finds the same days.
        let days = [
            day(1, 7000, 30),
            day(2, 6969, 6064),
            day(3, 11043, 10478),
            day(4, 7000, 40),
        ];
        for share in [0.02, 0.10, 0.25] {
            let found = windows(
                &days,
                Rule {
                    share,
                    ..Rule::default()
                },
            );
            assert_eq!(found.len(), 1, "share {share} found {found:?}");
            assert_eq!(found[0].days(), 2);
        }
    }

    #[test]
    fn two_rebuilds_in_one_range_stay_separate() {
        let mut days = vec![day(1, 7000, 10)];
        for n in 2..5 {
            days.push(day(n, 8000, 7000));
        }
        for n in 5..40 {
            days.push(day(n, 7000, 20));
        }
        for n in 40..44 {
            days.push(day(n, 8000, 7500));
        }
        let found = windows(&days, Rule::default());
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].days(), 3);
        assert_eq!(found[1].days(), 4);
    }
}
