// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Check what an RPM update does to the packages that depend on it:
//! which Provides it changes or drops, whether its own packages'
//! Requires still resolve, and which reverse dependencies would fail to
//! install (FTI) or to build (FTBFS). ebranch's `check-update` runs it
//! on Fedora and EPEL updates; a tool supplies the repositories through
//! [`RepoQuery`].

pub mod analysis;
pub mod arrival;
mod fedrq;
pub mod repo;
pub mod report;
pub mod rich;

pub use repo::RepoQuery;
