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

/// Generate a JSON Schema for the report format.
pub fn json_schema() -> String {
    let schema = schemars::schema_for!(HealthReport);
    serde_json::to_string_pretty(&schema).expect("schema serialization failed")
}

#[cfg(test)]
mod schema_tests {
    use super::*;

    /// Verify the checked-in JSON Schema matches the current model.
    ///
    /// To update the checked-in file:
    ///
    /// ```sh
    /// UPDATE_SCHEMA=1 cargo test -p sandogasa-pkg-health schema_up_to_date
    /// ```
    ///
    /// Review the diff before committing — new required fields are a
    /// breaking change, new optional fields are a minor change.
    #[test]
    fn schema_up_to_date() {
        let schema_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("health-report.schema.json");
        let generated = json_schema();

        if std::env::var("UPDATE_SCHEMA").is_ok() {
            std::fs::write(&schema_path, &generated).expect("failed to write schema");
            eprintln!("Updated {}", schema_path.display());
            return;
        }

        let committed = std::fs::read_to_string(&schema_path).unwrap_or_else(|_| {
            panic!(
                "Schema file not found at {}. Run:\n  \
                 UPDATE_SCHEMA=1 cargo test -p sandogasa-pkg-health schema_up_to_date",
                schema_path.display()
            )
        });

        if generated != committed {
            for (i, (a, b)) in generated.lines().zip(committed.lines()).enumerate() {
                if a != b {
                    panic!(
                        "Schema is out of date (first difference at line {}). Run:\n  \
                         UPDATE_SCHEMA=1 cargo test -p sandogasa-pkg-health schema_up_to_date\n\n\
                         expected: {a}\n  actual: {b}",
                        i + 1
                    );
                }
            }
            panic!(
                "Schema is out of date (line count differs). Run:\n  \
                 UPDATE_SCHEMA=1 cargo test -p sandogasa-pkg-health schema_up_to_date"
            );
        }
    }
}
