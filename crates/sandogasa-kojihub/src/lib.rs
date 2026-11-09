// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Koji hub XML-RPC client.
//!
//! Koji's hub speaks XML-RPC (there is no JSON API), so this crate
//! carries a minimal client: [`xmlrpc`] is the wire layer (a
//! `Value` tree over blocking reqwest + quick-xml, extracted from
//! koji-diff), and [`hub`] is a typed layer over the hub methods
//! the sandogasa tools use (`listTasks`, `getTaskInfo`,
//! `listHosts`, `listChannels`, `listBuildRPMs`, `getLatestBuilds`),
//! several at once through `multiCall`. [`config`] finds the hub a
//! `koji` CLI profile names.
//!
//! Anonymous calls work for all read methods used here — no Koji
//! credentials or the `koji` CLI are required.

pub mod config;
pub mod hub;
pub mod xmlrpc;

pub use hub::{HostConfig, HubClient, HubTask, ListTasksOpts, QueryOpts};
pub use xmlrpc::{Client, Error, Value};

/// Retry an operation with exponential backoff on retriable errors
/// (transport failures and 5xx; XML-RPC faults are not retried):
/// `retries` retries after the first attempt, through
/// [`sandogasa_cli::retry::retry_blocking`], which every client here
/// shares.
pub fn retry<T, F: Fn() -> Result<T, Error>>(retries: u32, f: F) -> Result<T, Error> {
    sandogasa_cli::retry::retry_blocking(
        "koji",
        &sandogasa_cli::retry::Policy::attempts(retries.saturating_add(1)),
        Error::is_retriable,
        f,
    )
}
