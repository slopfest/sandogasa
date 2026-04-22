// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared context passed to each HealthCheck.

/// Context bundles API clients so checks can reuse them across
/// packages without re-initializing. Add fields as checks need them.
#[derive(Default)]
pub struct Context {
    // Placeholder — fields added per-check.
    _private: (),
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }
}
