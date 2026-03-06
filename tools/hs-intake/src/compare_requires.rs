// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::compare::{self, CompareResult};
use crate::fedrq::Fedrq;

/// Compare the Requires of a source package between two branches.
pub fn compare_requires(
    srpm: &str,
    source_branch: &str,
    target_branch: &str,
) -> Result<CompareResult, crate::fedrq::Error> {
    let source_fq = Fedrq {
        branch: Some(source_branch.to_string()),
        ..Default::default()
    };
    let target_fq = Fedrq {
        branch: Some(target_branch.to_string()),
        ..Default::default()
    };

    let source = source_fq.subpkgs_requires(srpm)?;
    let target = target_fq.subpkgs_requires(srpm)?;

    Ok(compare::diff(source, target))
}
