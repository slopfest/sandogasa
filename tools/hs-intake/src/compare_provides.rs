// SPDX-License-Identifier: MPL-2.0

use crate::compare::{self, CompareResult};
use crate::fedrq::Fedrq;

/// Compare the Provides of a source package between two branches.
pub fn compare_provides(
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

    let source = source_fq.subpkgs_provides(srpm)?;
    let target = target_fq.subpkgs_provides(srpm)?;

    Ok(compare::diff(source, target))
}
