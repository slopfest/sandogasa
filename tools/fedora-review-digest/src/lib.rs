// SPDX-License-Identifier: Apache-2.0 OR MIT

//! fedora-review-digest — turn a `fedora-review` result for an
//! auto-generated spec (rust2rpm today; pyp2spec later) into the
//! condensed, rust-sig-style Bugzilla review comment: a short checklist
//! with a per-item verdict, plus the post-import task boilerplate. The
//! noise of a full fedora-review template is dropped because, for a
//! generated spec, most of it isn't decision-relevant.

pub mod checklist;
pub mod cratesio;
pub mod review;
