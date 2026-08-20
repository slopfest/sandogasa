// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Finding days when one architecture stopped being served.
//!
//! This is the second kind of window, and it is not a busy one. A mass
//! rebuild ([`crate::rebuild`]) is congestion: the load is enormous and the
//! queue drains slowly because of it. A stall is the opposite — ordinary
//! load, and one architecture simply not keeping up while the others are
//! served in a minute.
//!
//! The event that motivated it: s390x `buildArch` tasks created on
//! 2026-05-06 waited a mean of 46.0 hours, those created on 05-07 waited
//! 38.3, and 25 of them never ran at all — while on the worst of those days
//! x86_64, aarch64, ppc64le and i386 all sat at 1-2 minutes. There was no
//! mass rebuild that month and the week's volume was ordinary (371 s390x
//! tasks created on 05-06, against 291 and 260 the days before). Those two
//! days are the worst in fourteen months for the architecture the whole
//! question is about, five times worse than any rebuild day, and no
//! schedule names them.
//!
//! Waits are attributed to the day the work was *created*, which is the
//! honest bucket for "how long did what arrived today have to wait": the
//! backlog from 05-06 drained on 05-08, so bucketing by start time would
//! blame the day the queue recovered. (For capacity curves the opposite
//! holds — see `queries/arch-load-vs-wait.sql`, where creation-day
//! bucketing made quiet days look worst.)
//!
//! A stall inside a rebuild window is congestion and should be reported as
//! part of that rebuild; outside one it is an availability failure. This
//! module does not decide which — it finds the days, and the caller that
//! knows the rebuild windows says which kind each is.

/// One architecture's day: what arrived, and how long it waited.
#[derive(Debug, Clone, PartialEq)]
pub struct Day {
    /// UTC midnight of the creation day.
    pub at: f64,
    pub arch: String,
    /// Tasks created this day.
    pub created: usize,
    /// How many of them ever started.
    pub started: usize,
    /// Mean seconds those that started spent queued.
    pub wait: f64,
}

/// A stretch where one architecture lagged the rest.
#[derive(Debug, Clone, PartialEq)]
pub struct Stall {
    pub arch: String,
    /// First day, UTC midnight.
    pub from: f64,
    /// Exclusive end, so `to - from` is the length.
    pub to: f64,
    pub created: usize,
    /// Tasks created in the window that never started at all.
    pub never_started: usize,
    /// Worst daily mean wait, in seconds.
    pub worst: f64,
    /// The other architectures' median daily mean over the same days, in
    /// seconds — the "meanwhile, everything else" figure.
    pub others: f64,
}

impl Stall {
    pub fn days(&self) -> usize {
        ((self.to - self.from) / 86_400.0).round() as usize
    }

    /// How many times worse than the rest of the fleet.
    pub fn factor(&self) -> f64 {
        if self.others <= 0.0 {
            f64::INFINITY
        } else {
            self.worst / self.others
        }
    }
}

/// How much worse counts as a stall.
#[derive(Debug, Clone, Copy)]
pub struct Rule {
    /// How many times the other architectures' median a day must exceed.
    pub factor: f64,
    /// Minimum mean wait in seconds, so a fleet running at two seconds
    /// cannot produce a tenfold "stall" at twenty.
    pub floor: f64,
    /// Minimum tasks created, so an architecture with three tasks that day
    /// is not judged on them.
    pub min_tasks: usize,
}

impl Default for Rule {
    fn default() -> Self {
        // Tenfold with an hour's floor: the May event ran to 1,000x over a
        // fleet at 1-2 minutes, while ordinary variation between
        // architectures on a normal day is well inside 10x of a few
        // minutes, which the floor excludes anyway.
        Self {
            factor: 10.0,
            floor: 3_600.0,
            min_tasks: 50,
        }
    }
}

/// The stalls in `days`, one per architecture per consecutive stretch.
///
/// `days` may hold every architecture in any order; days are grouped by
/// date to compare each architecture against its peers on the same day.
pub fn stalls(days: &[Day], rule: Rule) -> Vec<Stall> {
    let mut by_day: std::collections::BTreeMap<i64, Vec<&Day>> = Default::default();
    for day in days {
        by_day.entry(day.at as i64).or_default().push(day);
    }

    // Per (arch, day): is it lagging, and by how much against its peers.
    let mut hot: std::collections::BTreeMap<&str, Vec<(f64, &Day, f64)>> = Default::default();
    for peers in by_day.values() {
        for day in peers {
            if day.created < rule.min_tasks || day.wait < rule.floor {
                continue;
            }
            // Median of the others, so one more stalled architecture in the
            // same incident does not mask either of them.
            let mut others: Vec<f64> = peers
                .iter()
                .filter(|p| p.arch != day.arch && p.created >= rule.min_tasks)
                .map(|p| p.wait)
                .collect();
            if others.is_empty() {
                continue;
            }
            others.sort_by(|a, b| a.total_cmp(b));
            let median = others[others.len() / 2];
            if day.wait >= median * rule.factor {
                hot.entry(&day.arch)
                    .or_default()
                    .push((day.at, day, median));
            }
        }
    }

    let mut found = Vec::new();
    for (arch, mut marked) in hot {
        marked.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut current: Option<Stall> = None;
        for (at, day, median) in marked {
            match &mut current {
                // Consecutive days extend the stretch; a gap closes it.
                Some(s) if (at - s.to).abs() < 1.0 => {
                    s.to = at + 86_400.0;
                    s.created += day.created;
                    s.never_started += day.created - day.started;
                    s.worst = s.worst.max(day.wait);
                    s.others = s.others.min(median);
                }
                _ => {
                    if let Some(s) = current.take() {
                        found.push(s);
                    }
                    current = Some(Stall {
                        arch: arch.to_string(),
                        from: at,
                        to: at + 86_400.0,
                        created: day.created,
                        never_started: day.created - day.started,
                        worst: day.wait,
                        others: median,
                    });
                }
            }
        }
        found.extend(current);
    }
    found.sort_by(|a, b| a.from.total_cmp(&b.from).then_with(|| a.arch.cmp(&b.arch)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: f64 = 3_600.0;

    fn day(n: i64, arch: &str, created: usize, started: usize, hours: f64) -> Day {
        Day {
            at: n as f64 * 86_400.0,
            arch: arch.to_string(),
            created,
            started,
            wait: hours * HOUR,
        }
    }

    /// A normal day: four architectures within minutes of each other.
    fn calm(n: i64) -> Vec<Day> {
        ["x86_64", "aarch64", "ppc64le", "s390x"]
            .iter()
            .map(|a| day(n, a, 400, 400, 0.03))
            .collect()
    }

    #[test]
    fn the_may_2026_shape_is_one_stall_of_two_days() {
        // s390x created 05-06 waited 46.0h and 05-07 waited 38.3h, 25 never
        // ran, while everything else was served in a minute.
        let mut days = calm(1);
        for (n, hours, created, started) in [(2, 46.0, 371, 358), (3, 38.3, 163, 151)] {
            days.extend(
                ["x86_64", "aarch64", "ppc64le"]
                    .iter()
                    .map(|a| day(n, a, 4000, 4000, 0.02)),
            );
            days.push(day(n, "s390x", created, started, hours));
        }
        days.extend(calm(4));

        let found = stalls(&days, Rule::default());
        assert_eq!(found.len(), 1, "{found:?}");
        let s = &found[0];
        assert_eq!(s.arch, "s390x");
        assert_eq!(s.days(), 2);
        assert_eq!(s.from, 2.0 * 86_400.0);
        assert_eq!(s.created, 534);
        assert_eq!(s.never_started, 25);
        assert_eq!(s.worst, 46.0 * HOUR);
        assert!(s.factor() > 100.0, "{}", s.factor());
    }

    #[test]
    fn a_calm_fleet_produces_nothing() {
        let days: Vec<Day> = (1..6).flat_map(calm).collect();
        assert!(stalls(&days, Rule::default()).is_empty());
    }

    #[test]
    fn a_fast_fleet_cannot_make_a_stall_out_of_seconds() {
        // Twentyfold, but twentyfold of two seconds. The floor exists so
        // ordinary jitter on a quiet day is not an incident.
        let mut days = calm(1);
        days.retain(|d| d.arch != "s390x");
        days.push(day(1, "s390x", 400, 400, 0.011));
        assert!(stalls(&days, Rule::default()).is_empty());
    }

    #[test]
    fn a_thin_architecture_is_not_judged_on_three_tasks() {
        let mut days = calm(1);
        days.retain(|d| d.arch != "s390x");
        // i386 gets a handful of builds and one of them waited all day.
        days.push(day(1, "i386", 4, 4, 20.0));
        assert!(stalls(&days, Rule::default()).is_empty());
        // And it cannot serve as the peer group either: with only a thin
        // architecture beside it, a genuinely slow one is left unjudged
        // rather than compared against noise.
        let sparse = vec![day(1, "s390x", 400, 400, 20.0), day(1, "i386", 4, 4, 0.02)];
        assert!(stalls(&sparse, Rule::default()).is_empty());
    }

    #[test]
    fn a_gap_splits_a_stall_in_two() {
        let mut days = Vec::new();
        for n in [1, 2, 5] {
            days.extend(
                ["x86_64", "aarch64", "ppc64le"]
                    .iter()
                    .map(|a| day(n, a, 4000, 4000, 0.02)),
            );
            days.push(day(n, "s390x", 300, 300, 30.0));
        }
        let found = stalls(&days, Rule::default());
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].days(), 2);
        assert_eq!(found[1].days(), 1);
    }

    #[test]
    fn two_architectures_stalling_together_are_both_reported() {
        // Comparing against the median of the others rather than the mean
        // keeps one incident from hiding the other.
        let mut days = vec![
            day(1, "x86_64", 4000, 4000, 0.02),
            day(1, "aarch64", 4000, 4000, 0.02),
            day(1, "ppc64le", 300, 300, 25.0),
            day(1, "s390x", 300, 300, 30.0),
        ];
        days.extend(calm(2));
        let found = stalls(&days, Rule::default());
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].arch, "ppc64le");
        assert_eq!(found[1].arch, "s390x");
    }

    #[test]
    fn a_rebuild_days_congestion_is_still_found_and_left_to_the_caller() {
        // F45's fallout: s390x at 79 minutes mean against a fleet at ~2.
        // That is congestion, not an outage, and this module reports it
        // either way — dating it against the rebuild windows is the
        // caller's job, which is why nothing here mentions releng.
        let mut days = vec![day(1, "s390x", 8212, 8212, 1.32)];
        days.extend(
            ["x86_64", "aarch64", "ppc64le"]
                .iter()
                .map(|a| day(1, a, 9000, 9000, 0.03)),
        );
        let found = stalls(&days, Rule::default());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].arch, "s390x");
        assert!(found[0].factor() > 10.0);
    }
}
