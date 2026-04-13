// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::collections::BTreeSet;

use crate::compare::{self, CompareResult};
use crate::fedrq::Fedrq;

/// Compare the BuildRequires of a source package between two branches.
///
/// Self-dependencies (build-requires on the srpm's own subpackages) are
/// filtered out.
pub fn compare_buildrequires(
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

    let source = source_fq.src_buildrequires(srpm)?;
    let target = target_fq.src_buildrequires(srpm)?;

    // Filter out self-dependencies on the srpm's own subpackages.
    let source_names = source_fq.subpkgs_names(srpm)?;
    let target_names = target_fq.subpkgs_names(srpm)?;
    let self_names: BTreeSet<String> = source_names.into_iter().chain(target_names).collect();

    let source = compare::filter_self_deps(source, &self_names);
    let target = compare::filter_self_deps(target, &self_names);

    Ok(compare::diff(source, target))
}
