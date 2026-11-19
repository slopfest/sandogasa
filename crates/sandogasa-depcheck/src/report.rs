// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What a check finds: the Provides an update changes, the Requires of
//! its own packages that do not resolve, and the reverse dependencies
//! it breaks at install time (FTI) or build time (FTBFS).

use serde::{Deserialize, Serialize};

use crate::arrival::ArrivalNote;

/// A Provide that changed between old and new versions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedProvide {
    /// The old Provide string (e.g. "tor = 0.4.9.5-1.el9").
    pub old: String,
    /// The new Provide string, if the name still exists.
    pub new: Option<String>,
}

impl ChangedProvide {
    /// Whether this is a version update (name still provided)
    /// vs a true removal.
    pub fn is_updated(&self) -> bool {
        self.new.is_some()
    }
}

/// A source package's version change in the update — its stable V-R
/// (`None` when the package is newly introduced) and the V-R the update
/// ships. Drives the grouped-by-version-transition summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageChange {
    pub package: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    pub new: String,
}

/// How a reverse dependency breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BreakKind {
    /// A binary subpackage's install-time Requires stops resolving —
    /// the package fails to install (FTI) once the update ships.
    Fti,
    /// The source package's BuildRequires stops resolving — the
    /// package fails to build from source (FTBFS) on its next
    /// rebuild.
    Ftbfs,
}

impl BreakKind {
    /// The label shown in reports.
    pub fn label(self) -> &'static str {
        match self {
            BreakKind::Fti => "FTI",
            BreakKind::Ftbfs => "FTBFS",
        }
    }
}

/// A single broken Requires entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokenRequires {
    /// The Requires string that would break.
    pub dep: String,
    /// The changed Provide(s) behind the break.
    pub changed_provide: String,
    /// Install-time (FTI) or build-time (FTBFS) breakage.
    pub kind: BreakKind,
}

/// An unsatisfied Requires in an updated package's subpackages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsatisfiedDep {
    /// The source package.
    pub package: String,
    /// The unsatisfied Requires string.
    pub dep: String,
    /// For a boolean/rich dep, the specific capabilities that didn't
    /// resolve (so the reader can see *which* part is the problem).
    /// Empty for a plain dep (where `dep` is itself the capability).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<String>,
}

/// Result of checking a single reverse dependency.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RevDepResult {
    /// "ok" or "broken".
    pub status: String,
    /// Broken Requires, if any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<BrokenRequires>,
    /// For a broken one, when it reached the target relative to the
    /// side tag (#83); `None` without a side tag, or when Koji could
    /// not say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arrival: Option<ArrivalNote>,
}
