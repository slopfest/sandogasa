// SPDX-License-Identifier: Apache-2.0 OR MIT

pub mod diff;
pub mod koji;
pub mod parse;

/// The XML-RPC layer moved to the shared
/// [sandogasa-kojihub](https://crates.io/crates/sandogasa-kojihub)
/// crate; these re-exports keep every `koji_diff::xmlrpc::*` path
/// compiling unchanged.
///
/// Note: cargo-semver-checks flags cross-crate re-exports as
/// "missing" items (it doesn't inline foreign items and compares
/// by item kind), so it reports this module as a major break.
/// That's a false positive for source compatibility — all prior
/// paths, constructors, and variant matches compile against the
/// re-exported types, as this crate's own bin and tests exercise.
pub mod xmlrpc {
    pub use sandogasa_kojihub::xmlrpc::{Client, Error, Value, parse_response};
}
