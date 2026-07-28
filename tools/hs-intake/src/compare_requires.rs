// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::compare::{self, CompareResult};
use crate::fedrq::Fedrq;

/// Compare the Requires of a source package between two branches.
///
/// Self-dependencies (requires on the srpm's own subpackages) are
/// filtered out.
pub fn compare_requires(
    srpm: &str,
    source_branch: &str,
    target_branch: &str,
) -> Result<CompareResult, crate::fedrq::Error> {
    compare::compare_attr(
        srpm,
        source_branch,
        target_branch,
        Fedrq::subpkgs_requires,
        true,
    )
}
