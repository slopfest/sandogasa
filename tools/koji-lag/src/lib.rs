// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Quantify Koji build queue lag and per-arch build-time drag.
//!
//! Fedora's primary architectures build in lockstep, so one slow
//! or queue-starved architecture delays every build — and scratch
//! builds, which gate dist-git PR CI, run at lower priority still.
//! This crate sweeps Koji task metadata (via anonymous hub
//! XML-RPC), stores it in a mergeable dataset so independently
//! collected runs can be pooled, and reports per-arch queue-wait /
//! build-time distributions plus critical-path attribution (which
//! arch finished last, and how much later than the runner-up).

pub mod dataset;
pub mod fetch;
pub mod instance;
pub mod report;
pub mod stats;

/// JSON Schema for the dataset format, for external consumers.
pub fn json_schema() -> String {
    let schema = schemars::schema_for!(dataset::Dataset);
    serde_json::to_string_pretty(&schema).expect("schema serializes")
}

#[cfg(test)]
mod tests {
    /// Snapshot test: the checked-in schema must match the code.
    /// Regenerate with `UPDATE_SCHEMA=1 cargo test -p koji-lag
    /// schema_up_to_date`.
    #[test]
    fn schema_up_to_date() {
        let expected = super::json_schema();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data/koji-lag-dataset.schema.json");
        if std::env::var("UPDATE_SCHEMA").is_ok() {
            std::fs::write(&path, format!("{expected}\n")).unwrap();
            return;
        }
        let on_disk = std::fs::read_to_string(&path)
            .expect("schema file missing; run UPDATE_SCHEMA=1 cargo test");
        assert_eq!(
            on_disk.trim_end(),
            expected,
            "schema drift; run UPDATE_SCHEMA=1 cargo test -p koji-lag"
        );
    }
}
