// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where a CVE is fixed, and whether a build has that fix.
//!
//! NVD's CPE data is the authoritative source of fixed versions and
//! vulnerable ranges; GitHub's advisory database fills in when NVD
//! has not analyzed a CVE yet; a range closed at the top with no fix
//! recorded says everything above its bound is not affected. This
//! crate gathers those (`facts`), paces and caches the NVD requests
//! (`cache`), reads advisories (`advisory`), compares versions
//! (`version`), and answers the one question its users share: is
//! this build a fix for that CVE. fedora-cve-triage asks it to close
//! bugs a Bodhi update already fixed; ebranch asks it to say which
//! CVE bugs a security update closes.

pub mod advisory;
pub mod cache;
pub mod facts;
pub mod version;

pub use cache::NvdCache;
pub use facts::{CveFacts, CveFix, fix_facts, is_fix, product_matches_component};
pub use sandogasa_nvd::{FixedVersion, VulnerableRange};
pub use version::{Nvr, version_gte};
