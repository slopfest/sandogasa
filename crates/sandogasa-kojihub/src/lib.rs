// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Koji hub XML-RPC client.
//!
//! Koji's hub speaks XML-RPC (there is no JSON API), so this crate
//! carries a minimal client: [`xmlrpc`] is the wire layer (a
//! `Value` tree over blocking reqwest + quick-xml, extracted from
//! koji-diff), and [`hub`] is a typed layer over the hub methods
//! the sandogasa tools use (`listTasks`, `getTaskInfo`,
//! `listHosts`, `listChannels`).
//!
//! Anonymous calls work for all read methods used here — no Koji
//! credentials or the `koji` CLI are required.

pub mod hub;
pub mod xmlrpc;

pub use hub::{HubClient, HubTask, ListTasksOpts, QueryOpts};
pub use xmlrpc::{Client, Error, Value};

/// Retry an operation with exponential backoff on retriable errors
/// (transport failures and 5xx; XML-RPC faults are not retried).
pub fn retry<T, F: Fn() -> Result<T, Error>>(retries: u32, f: F) -> Result<T, Error> {
    let mut last_err = None;
    for attempt in 0..=retries {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempt < retries && e.is_retriable() {
                    let delay = std::time::Duration::from_secs(1 << attempt);
                    eprintln!(
                        "  retrying in {}s ({}/{retries}): {e}",
                        delay.as_secs(),
                        attempt + 1,
                    );
                    std::thread::sleep(delay);
                    last_err = Some(e);
                } else {
                    return Err(e);
                }
            }
        }
    }
    Err(last_err.unwrap())
}
