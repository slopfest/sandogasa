// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Audit package health across a sandogasa inventory.
//!
//! See PLAN.md in the crate root for architecture and scope.

pub mod check;
pub mod checks;
pub mod context;
pub mod duration;
pub mod registry;
pub mod report;

pub use check::{CheckResult, CostTier, HealthCheck, entry_key};
pub use context::Context;
pub use registry::Registry;
pub use report::{CheckEntry, HealthReport, PackageReport};
