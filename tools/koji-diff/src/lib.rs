// SPDX-License-Identifier: Apache-2.0 OR MIT

pub mod diff;
pub mod koji;
pub mod parse;
// The XML-RPC layer moved to the shared sandogasa-kojihub crate;
// the re-export keeps `koji_diff::xmlrpc::*` paths working.
pub use sandogasa_kojihub::xmlrpc;
