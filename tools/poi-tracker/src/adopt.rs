// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `adopt` subcommand.
//!
//! The action counterpart to sandogasa-pkg-health's orphaned flag:
//! walk the inventory, find packages whose dist-git owner is the
//! `orphan` sentinel user, show each one's orphaning reason, and —
//! on confirmation — take ownership via the dist-git plugin's
//! take-orphan endpoint (the API behind the web UI's "Take"
//! button). An orphaned package is retired ~6 weeks after
//! orphaning unless someone adopts it, so this is how an
//! inventory's packages are kept from lapsing.
//!
//! Adoption is a real commitment, so unlike the batch triage
//! flows each package is confirmed individually (default no);
//! `-y` adopts every match without prompting and `--dry-run`
//! only reports. Packages already marked retired or unshipped in
//! the inventory are skipped — the endpoint refuses retired
//! packages anyway (those need a releng ticket).

use sandogasa_distgit::DistGitClient;
use sandogasa_inventory::Inventory;

use crate::triage_retired::{RETRY_ATTEMPTS, retry};

/// One orphaned package found in the inventory.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub package: String,
    /// Orphaning reason category (may be empty when none was
    /// recorded or the lookup failed).
    pub reason: String,
    /// Free-text detail accompanying the reason.
    pub reason_info: String,
}

impl Candidate {
    /// One-line description for prompts and reports.
    pub fn describe(&self) -> String {
        match (self.reason.is_empty(), self.reason_info.is_empty()) {
            (true, _) => self.package.clone(),
            (false, true) => format!("{} (orphaned: {})", self.package, self.reason),
            (false, false) => format!(
                "{} (orphaned: {} — {})",
                self.package, self.reason, self.reason_info
            ),
        }
    }
}

/// Summary returned from `run` so the caller can pick an exit
/// code without re-counting.
#[derive(Debug, Default)]
pub struct RunReport {
    pub packages_checked: usize,
    pub orphaned_found: usize,
    pub adopted: usize,
    pub failures: usize,
}

/// Run the whole `adopt` flow. `username` is the token's owner
/// (from `verify_token`), shown in prompts so it's obvious who
/// becomes the point of contact.
pub async fn run(
    inventory: &Inventory,
    dg: &DistGitClient,
    username: &str,
    filter: &crate::WalkFilterArgs,
    dry_run: bool,
    yes: bool,
    verbose: bool,
) -> Result<RunReport, String> {
    let mut report = RunReport::default();
    let mut candidates: Vec<Candidate> = Vec::new();

    for pkg in &inventory.package {
        if !filter.matches(&pkg.name) {
            continue;
        }
        // Retired packages can't be taken over the API (they need
        // a releng ticket); unshipped ones have nothing to adopt.
        if pkg.is_unshipped() || pkg.is_retired_on("rawhide") {
            if verbose {
                eprintln!(
                    "[poi-tracker] {}: marked retired/unshipped in the \
                     inventory; skipping",
                    pkg.name
                );
            }
            continue;
        }
        report.packages_checked += 1;
        if verbose {
            eprintln!("[poi-tracker] {}: checking dist-git owner", pkg.name);
        }
        let acls = match retry(
            &format!("ACLs for {}", pkg.name),
            RETRY_ATTEMPTS,
            || dg.get_acls(&pkg.name),
            verbose,
        )
        .await
        {
            Ok(a) => a,
            Err(e) => {
                // A missing answer must not be mistaken for "not
                // orphaned" — but it shouldn't kill the walk either.
                eprintln!("warning: {}: could not check owner: {e}", pkg.name);
                report.failures += 1;
                continue;
            }
        };
        if !acls.access_users.owner.iter().any(|u| u == "orphan") {
            continue;
        }
        report.orphaned_found += 1;

        // Reason is best-effort color for the prompt: a failed
        // lookup mustn't block adoption.
        let (reason, reason_info) = match dg.orphan_info(&pkg.name).await {
            Ok(info) => (info.reason, info.reason_info),
            Err(e) => {
                eprintln!("warning: {}: could not fetch orphan reason: {e}", pkg.name);
                (String::new(), String::new())
            }
        };
        let candidate = Candidate {
            package: pkg.name.clone(),
            reason,
            reason_info,
        };
        println!("orphaned: {}", candidate.describe());
        candidates.push(candidate);
    }

    if candidates.is_empty() {
        println!("No orphaned packages in the inventory.");
        return Ok(report);
    }
    if dry_run {
        eprintln!("\n(dry-run: not adopting)");
        return Ok(report);
    }

    // Adoption is per-package: taking ownership is a commitment,
    // so no batch yes/no over the whole list. `-y` opts into all.
    for candidate in &candidates {
        if !yes {
            let adopt = crate::triage_updates::confirm(&format!(
                "Adopt {} as {username}?",
                candidate.describe()
            ))?;
            if !adopt {
                continue;
            }
        }
        match dg.take_orphan(&candidate.package).await {
            Ok(poc) => {
                report.adopted += 1;
                eprintln!("adopted {}: point of contact now {poc}", candidate.package);
            }
            Err(e) => {
                report.failures += 1;
                eprintln!("error: {}: {e}", candidate.package);
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use sandogasa_distgit::DistGitClient;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn inventory(names: &[&str]) -> Inventory {
        let mut toml =
            String::from("[inventory]\nname = \"t\"\ndescription = \"t\"\nmaintainer = \"t\"\n");
        for name in names {
            toml.push_str(&format!("\n[[package]]\nname = \"{name}\"\n"));
        }
        toml::from_str(&toml).unwrap()
    }

    fn acls_json(owner: &str) -> serde_json::Value {
        serde_json::json!({
            "access_users": {
                "owner": [owner],
                "admin": [], "commit": [], "collaborator": [], "ticket": []
            },
            "access_groups": {
                "admin": [], "commit": [], "collaborator": [], "ticket": []
            },
            "name": "x",
            "namespace": "rpms"
        })
    }

    async fn mount_pkg(server: &MockServer, pkg: &str, owner: &str) {
        Mock::given(method("GET"))
            .and(path(format!("/api/0/rpms/{pkg}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(acls_json(owner)))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn adopts_orphaned_package_under_yes() {
        let server = MockServer::start().await;
        mount_pkg(&server, "ccze", "orphan").await;
        mount_pkg(&server, "bash", "alice").await;
        Mock::given(method("GET"))
            .and(path("/_dg/orphan/rpms/ccze"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "orphan": true, "reason": "Lack of time", "reason_info": ""
            })))
            .mount(&server)
            .await;
        // Only the orphaned package is taken.
        Mock::given(method("POST"))
            .and(path("/_dg/take_orphan/rpms/ccze"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "point_of_contact": "salimma"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let inv = inventory(&["ccze", "bash"]);
        let dg = DistGitClient::with_base_url(&server.uri()).with_token("tok".to_string());
        let report = run(
            &inv,
            &dg,
            "salimma",
            &crate::WalkFilterArgs::default(),
            false,
            true,
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.packages_checked, 2);
        assert_eq!(report.orphaned_found, 1);
        assert_eq!(report.adopted, 1);
        assert_eq!(report.failures, 0);
    }

    #[tokio::test]
    async fn dry_run_never_posts() {
        let server = MockServer::start().await;
        mount_pkg(&server, "ccze", "orphan").await;
        Mock::given(method("GET"))
            .and(path("/_dg/orphan/rpms/ccze"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "orphan": true, "reason": "", "reason_info": ""
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/_dg/take_orphan/rpms/ccze"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(0)
            .mount(&server)
            .await;

        let inv = inventory(&["ccze"]);
        let dg = DistGitClient::with_base_url(&server.uri()).with_token("tok".to_string());
        let report = run(
            &inv,
            &dg,
            "salimma",
            &crate::WalkFilterArgs::default(),
            true,
            true,
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.orphaned_found, 1);
        assert_eq!(report.adopted, 0);
    }

    #[tokio::test]
    async fn retired_marked_package_is_skipped_without_requests() {
        // No server: a request for the retired package would error
        // and bump failures.
        let inv: Inventory = toml::from_str(
            "[inventory]\nname = \"t\"\ndescription = \"t\"\nmaintainer = \"t\"\n\
             \n[[package]]\nname = \"dead\"\nretired_on = [\"rawhide\"]\n",
        )
        .unwrap();
        let dg = DistGitClient::with_base_url("http://127.0.0.1:1");
        let report = run(
            &inv,
            &dg,
            "salimma",
            &crate::WalkFilterArgs::default(),
            false,
            true,
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.packages_checked, 0);
        assert_eq!(report.failures, 0);
    }

    #[tokio::test]
    async fn take_failure_counts_and_continues() {
        let server = MockServer::start().await;
        mount_pkg(&server, "ccze", "orphan").await;
        mount_pkg(&server, "colorized-logs", "orphan").await;
        for pkg in ["ccze", "colorized-logs"] {
            Mock::given(method("GET"))
                .and(path(format!("/_dg/orphan/rpms/{pkg}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "orphan": true, "reason": "", "reason_info": ""
                })))
                .mount(&server)
                .await;
        }
        Mock::given(method("POST"))
            .and(path("/_dg/take_orphan/rpms/ccze"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": "You must be a packager to adopt a package."
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/_dg/take_orphan/rpms/colorized-logs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "point_of_contact": "salimma"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let inv = inventory(&["ccze", "colorized-logs"]);
        let dg = DistGitClient::with_base_url(&server.uri()).with_token("tok".to_string());
        let report = run(
            &inv,
            &dg,
            "salimma",
            &crate::WalkFilterArgs::default(),
            false,
            true,
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.orphaned_found, 2);
        assert_eq!(report.adopted, 1);
        assert_eq!(report.failures, 1);
    }
}
