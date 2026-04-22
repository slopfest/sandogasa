// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Registry of available health checks.

use crate::check::{CostTier, HealthCheck};

/// A registry of checks available to the tool. Built once at startup.
pub struct Registry {
    checks: Vec<Box<dyn HealthCheck>>,
}

impl Registry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self { checks: Vec::new() }
    }

    /// Register a check.
    pub fn register(&mut self, check: Box<dyn HealthCheck>) {
        self.checks.push(check);
    }

    /// Iterate over all registered checks.
    pub fn all(&self) -> impl Iterator<Item = &dyn HealthCheck> {
        self.checks.iter().map(|c| c.as_ref())
    }

    /// Find a check by id.
    pub fn get(&self, id: &str) -> Option<&dyn HealthCheck> {
        self.checks
            .iter()
            .map(|c| c.as_ref())
            .find(|c| c.id() == id)
    }

    /// Iterate over checks matching the given cost tier.
    pub fn by_tier(&self, tier: CostTier) -> impl Iterator<Item = &dyn HealthCheck> {
        self.checks
            .iter()
            .map(|c| c.as_ref())
            .filter(move |c| c.cost_tier() == tier)
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the default registry with all MVP checks wired in.
pub fn default_registry() -> Registry {
    let mut reg = Registry::new();
    reg.register(Box::new(crate::checks::maintainer_count::MaintainerCount));
    reg.register(Box::new(crate::checks::bug_count::BugCount));
    reg
}
