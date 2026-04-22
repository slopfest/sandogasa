// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Classify issue-tracker bugs into a portable set of categories.
//!
//! The [`BugKind`] enum is the shared vocabulary across trackers —
//! CVEs, FTBFS / FTI, update requests, etc. Per-tracker submodules
//! implement the classification logic for their tracker's specific
//! conventions (keywords, aliases, blocks relationships, labels,
//! etc.). Currently only Bugzilla is supported; GitLab / GitHub /
//! others can be added alongside as new submodules.

pub mod bugzilla;

/// Tracker-agnostic bug category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BugKind {
    /// Package review request.
    Review,
    /// Security issue (CVE or equivalent).
    Security,
    /// Update request ("X is available").
    Update,
    /// Branch request for a downstream distribution.
    Branch,
    /// Fails to build from source.
    Ftbfs,
    /// Fails to install.
    Fti,
    /// Doesn't fit any of the above.
    Other,
}

impl BugKind {
    /// Return the short string id for this kind (stable identifier
    /// suitable for serialization and reports).
    pub fn as_str(&self) -> &'static str {
        match self {
            BugKind::Review => "review",
            BugKind::Security => "security",
            BugKind::Update => "update",
            BugKind::Branch => "branch",
            BugKind::Ftbfs => "ftbfs",
            BugKind::Fti => "fti",
            BugKind::Other => "other",
        }
    }
}
