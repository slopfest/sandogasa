// SPDX-License-Identifier: Apache-2.0 OR MIT

//! When a package reached a release, relative to a side tag: whether it
//! was there when the side tag was created, or arrived since, as a new
//! package or a rebuild. A rebuild campaign scoped when the side tag
//! opened cannot have planned for what arrived later (#83, #84).

use serde::Serialize;

/// When a package reached the target, relative to the side tag's
/// creation: what tells a reverse dependency the side tag has to catch
/// up with from one it can leave as it is (#83).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Arrival {
    /// The package entered the distribution after the side tag was
    /// created, so no plan made before then could list it.
    NewPackage,
    /// An existing package whose newest build in the target finished
    /// after the side tag was created.
    Rebuilt,
    /// Neither: it was there, as it is, when the side tag was opened.
    PreExisting,
}

/// A reverse dependency's [`Arrival`], with the day and the build that
/// show it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ArrivalNote {
    pub arrival: Arrival,
    /// The day it arrived (`YYYY-MM-DD`): first listed, or rebuilt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    /// The target's build, for one rebuilt since.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nvr: Option<String>,
}

/// What Koji says of a package's history in a target: when it was first
/// added to any tag's package list, and its newest build in the target
/// with when that build finished (unix times).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackageHistory {
    pub first_listed: Option<f64>,
    pub latest: Option<(String, Option<f64>)>,
}

/// A unix time as the day it falls on, in UTC.
pub(crate) fn day_of(ts: f64) -> Option<String> {
    chrono::DateTime::from_timestamp(ts as i64, 0).map(|t| t.format("%Y-%m-%d").to_string())
}

/// When a reverse dependency reached the target, relative to a side tag
/// created at `opened` (#83). A package first listed anywhere after
/// then is new: the first listing across every tag, not the target's,
/// so a release branched while the side tag was open does not make
/// every package look new. Otherwise one whose newest build in the
/// target finished after then was rebuilt. `None` when Koji said
/// nothing of it.
pub fn arrival(opened: f64, h: &PackageHistory) -> Option<ArrivalNote> {
    if h.first_listed.is_none() && h.latest.is_none() {
        return None;
    }
    if let Some(first) = h.first_listed.filter(|f| *f > opened) {
        return Some(ArrivalNote {
            arrival: Arrival::NewPackage,
            on: day_of(first),
            nvr: None,
        });
    }
    if let Some((nvr, Some(done))) = &h.latest
        && *done > opened
    {
        return Some(ArrivalNote {
            arrival: Arrival::Rebuilt,
            on: day_of(*done),
            nvr: Some(nvr.clone()),
        });
    }
    Some(ArrivalNote {
        arrival: Arrival::PreExisting,
        on: None,
        nvr: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-02 08:40 UTC, when f46-build-side-158330 was created.
    const OPENED: f64 = 1_793_608_815.5;

    #[test]
    fn a_reverse_dependency_is_new_rebuilt_or_already_there() {
        // ical: first listed in f46 on 2026-11-05.
        let ical = PackageHistory {
            first_listed: Some(1_793_896_312.3),
            latest: Some(("ical-4.0.2-1.fc46".into(), Some(1_793_898_192.7))),
        };
        assert_eq!(
            arrival(OPENED, &ical),
            Some(ArrivalNote {
                arrival: Arrival::NewPackage,
                on: Some("2026-11-05".into()),
                nvr: None,
            })
        );
        // Listed since 2007, rebuilt in the target on 2026-11-06.
        let rebuilt = PackageHistory {
            first_listed: Some(1_185_276_321.6),
            latest: Some(("asterisk-24.0.0-3.fc46".into(), Some(1_793_959_200.5))),
        };
        assert_eq!(
            arrival(OPENED, &rebuilt),
            Some(ArrivalNote {
                arrival: Arrival::Rebuilt,
                on: Some("2026-11-06".into()),
                nvr: Some("asterisk-24.0.0-3.fc46".into()),
            })
        );
        // kdepimlibs: listed since 2007, last built in August.
        let kdepimlibs = PackageHistory {
            first_listed: Some(1_185_276_321.6),
            latest: Some(("kdepimlibs-4.14.10-49.fc43".into(), Some(1_786_000_000.0))),
        };
        assert_eq!(
            arrival(OPENED, &kdepimlibs).unwrap().arrival,
            Arrival::PreExisting
        );
        // Nothing known: no label.
        assert_eq!(arrival(OPENED, &PackageHistory::default()), None);
    }
}
