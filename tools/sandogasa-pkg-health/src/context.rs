// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared context passed to each HealthCheck.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use sandogasa_bugclass::bugzilla::TrackerIds;
use sandogasa_bugzilla::BzClient;
use sandogasa_distgit::DistGitClient;
use tokio::runtime::Handle;

const BUGZILLA_URL: &str = "https://bugzilla.redhat.com";

/// Context bundles API clients and a tokio runtime handle so
/// checks can reuse them across packages without re-initializing.
///
/// The trait is synchronous to stay trait-object friendly; checks
/// that need async work call `ctx.block_on(future)`.
pub struct Context {
    /// Tokio runtime handle for block_on.
    pub runtime: Handle,
    /// Dist-git (Pagure) client for ACL and group queries.
    pub distgit: Arc<DistGitClient>,
    /// Bugzilla client for bug queries.
    pub bz: Arc<BzClient>,
    /// Fedora versions the user requested (for variant-aware checks).
    /// Rawhide is implicit and always available in `trackers`.
    pub fedora_versions: Vec<u32>,
    /// EPEL versions the user requested.
    pub epel_versions: Vec<u32>,
    /// FTBFS / FTI tracker bug IDs per version key. Keys are
    /// `"rawhide"`, `"f44"`, `"epel9"`, etc. Populated at startup.
    pub trackers: BTreeMap<String, Arc<TrackerIds>>,
}

impl Context {
    /// Build a Context with default clients. Must be called from
    /// within a tokio runtime. Looks up FTBFS/FTI trackers once.
    pub async fn new(fedora_versions: &[u32], epel_versions: &[u32], verbose: bool) -> Self {
        let bz = Arc::new(BzClient::new(BUGZILLA_URL));

        if verbose {
            eprintln!("[pkg-health] looking up FTBFS/FTI tracker bugs");
        }

        let mut trackers: BTreeMap<String, Arc<TrackerIds>> = BTreeMap::new();
        trackers.insert(
            "rawhide".to_string(),
            Arc::new(fetch_version_trackers(&bz, "RAWHIDE").await),
        );
        for &ver in fedora_versions {
            trackers.insert(
                format!("f{ver}"),
                Arc::new(fetch_version_trackers(&bz, &format!("F{ver}")).await),
            );
        }
        for &ver in epel_versions {
            trackers.insert(
                format!("epel{ver}"),
                Arc::new(fetch_version_trackers(&bz, &format!("EPEL{ver}")).await),
            );
        }

        Self {
            runtime: Handle::current(),
            distgit: Arc::new(DistGitClient::new()),
            bz,
            fedora_versions: fedora_versions.to_vec(),
            epel_versions: epel_versions.to_vec(),
            trackers,
        }
    }

    /// Block on a future using the stored runtime handle. Uses
    /// `block_in_place` to avoid deadlocking when called from within
    /// the runtime.
    pub fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        tokio::task::block_in_place(|| self.runtime.block_on(future))
    }
}

/// Look up FTBFS/FTI tracker IDs for a single version prefix (e.g.
/// "RAWHIDE", "F45"). Returns empty if no matching trackers found.
async fn fetch_version_trackers(bz: &BzClient, prefix: &str) -> TrackerIds {
    let aliases = [
        format!("alias={prefix}FTBFS"),
        format!("alias={prefix}FailsToInstall"),
    ];
    let query = aliases.join("&");
    let mut ftbfs = HashSet::new();
    let mut fti = HashSet::new();
    if let Ok(bugs) = bz.search(&query, 0).await {
        for bug in &bugs {
            for alias in &bug.alias {
                if alias.ends_with("FTBFS") {
                    ftbfs.insert(bug.id);
                } else if alias.ends_with("FailsToInstall") {
                    fti.insert(bug.id);
                }
            }
        }
    }
    TrackerIds { ftbfs, fti }
}
