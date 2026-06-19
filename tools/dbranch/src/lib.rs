// SPDX-License-Identifier: Apache-2.0 OR MIT

//! dbranch — propagate a Debian package across its Ubuntu/PPA
//! branches: merge the Debian branch, resolve the changelog
//! conflict, regenerate the `~<codename>+<N>` rebuild entry, and
//! scratch-build.

pub mod changelog;
pub mod distroinfo;
pub mod gbpconf;
pub mod git;
pub mod host;
pub mod plan;
pub mod rebuild;
pub mod salsaci;
pub mod ui;
