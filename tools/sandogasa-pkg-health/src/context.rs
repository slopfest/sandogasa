// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared context passed to each HealthCheck.

use std::sync::Arc;

use sandogasa_distgit::DistGitClient;
use tokio::runtime::Handle;

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
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

impl Context {
    /// Create a Context with default clients. Must be called from
    /// within a tokio runtime.
    pub fn new() -> Self {
        Self {
            runtime: Handle::current(),
            distgit: Arc::new(DistGitClient::new()),
        }
    }

    /// Block on a future using the stored runtime handle.
    pub fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        // `Handle::block_on` is not available from within the same
        // runtime; spawn a thread to do the block to avoid deadlock.
        tokio::task::block_in_place(|| self.runtime.block_on(future))
    }
}
