// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Prune a staging COPR: which of its packages every target release
//! has caught up on, and — on confirmation — delete them, so the COPR
//! keeps only what is still in flight.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use sandogasa_copr::PackageStatus;
use serde::Serialize;

/// What to prune and how.
pub struct Options {
    /// `@rust` for a group project.
    pub owner: String,
    pub project: String,
    /// Delete every caught-up package without asking.
    pub yes: bool,
    /// Print the plan as JSON; never deletes.
    pub json: bool,
    pub verbose: bool,
    /// Ask before each deletion (a terminal, no `--json`).
    pub interactive: bool,
}

/// How one target release stands against the COPR's build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "verdict")]
pub enum Verdict {
    /// The branch carries the COPR's version or newer.
    CaughtUp { branch_vr: String },
    /// The branch has the package, older than the COPR's build.
    Behind { branch_vr: String },
    /// The branch does not have the package at all.
    Absent,
    /// The COPR's latest build did not succeed there.
    NotBuilt { state: String },
}

/// One package's standing in one target release.
#[derive(Debug, Clone, Serialize)]
pub struct BranchStanding {
    pub branch: String,
    /// The COPR's `version-release`, when its build succeeded.
    pub copr_vr: Option<String>,
    #[serde(flatten)]
    pub verdict: Verdict,
}

/// One package of the COPR and whether it can go.
#[derive(Debug, Clone, Serialize)]
pub struct PackagePlan {
    pub name: String,
    /// Every target release has caught up: nothing here is in flight.
    pub prunable: bool,
    pub branches: Vec<BranchStanding>,
}

/// The whole plan, as `--json` prints it.
#[derive(Debug, Serialize)]
pub struct Report {
    pub copr: String,
    pub packages: Vec<PackagePlan>,
    /// Chroots whose branch could not be named (skipped).
    pub skipped_chroots: Vec<String>,
}

/// `(branch, source name)` → `version-release` on that branch.
pub type BranchVersions = BTreeMap<(String, String), String>;

/// The branch a COPR chroot builds for: `fedora-rawhide-x86_64` →
/// `rawhide`, `fedora-46-*` → `f46`, `epel-9-*` → `epel9`,
/// `centos-stream-10-*` → `c10s`, `fedora-eln-*` → `eln`. The inverse
/// of [`sandogasa_copr::chroot_prefix`].
pub fn chroot_branch(chroot: &str) -> Option<String> {
    let (distro, rest) = chroot.rsplit_once('-')?; // drop the arch
    if let Some(n) = distro.strip_prefix("fedora-") {
        return Some(match n {
            "rawhide" | "eln" => n.to_string(),
            n if n.chars().all(|c| c.is_ascii_digit()) => format!("f{n}"),
            _ => return None,
        });
    }
    if let Some(n) = distro.strip_prefix("epel-")
        && n.chars().all(|c| c.is_ascii_digit())
    {
        return Some(format!("epel{n}"));
    }
    if let Some(n) = distro.strip_prefix("centos-stream-")
        && n.chars().all(|c| c.is_ascii_digit())
    {
        return Some(format!("c{n}s"));
    }
    let _ = rest;
    None
}

/// The COPR's representative build per branch: the x86_64 chroot when
/// there is one, else the first by name. Returns `(state, V-R)` with
/// any epoch prefix kept for the comparison.
fn copr_build_for_branch<'a>(
    pkg: &'a PackageStatus,
    branch: &str,
) -> Option<(&'a str, Option<&'a str>)> {
    pkg.chroots
        .iter()
        .filter(|(chroot, _)| chroot_branch(chroot).as_deref() == Some(branch))
        .min_by_key(|(chroot, _)| !chroot.ends_with("-x86_64"))
        .map(|(_, s)| (s.state.as_str(), s.pkg_version.as_deref()))
}

/// Decide every package against every branch it builds for.
pub fn plan(
    packages: &[PackageStatus],
    branches: &[String],
    versions: &BranchVersions,
) -> Vec<PackagePlan> {
    let mut out: Vec<PackagePlan> = packages
        .iter()
        .map(|pkg| {
            let standings: Vec<BranchStanding> = branches
                .iter()
                .filter_map(|branch| {
                    let (state, vr) = copr_build_for_branch(pkg, branch)?;
                    let branch_vr = versions.get(&(branch.clone(), pkg.name.clone()));
                    let verdict = match (state, vr, branch_vr) {
                        ("succeeded", Some(copr_vr), Some(b)) => {
                            if sandogasa_rpmvercmp::compare_evr(b, copr_vr) != Ordering::Less {
                                Verdict::CaughtUp {
                                    branch_vr: b.clone(),
                                }
                            } else {
                                Verdict::Behind {
                                    branch_vr: b.clone(),
                                }
                            }
                        }
                        ("succeeded", Some(_), None) => Verdict::Absent,
                        (state, _, _) => Verdict::NotBuilt {
                            state: state.to_string(),
                        },
                    };
                    Some(BranchStanding {
                        branch: branch.clone(),
                        copr_vr: (state == "succeeded").then(|| vr.unwrap_or("").to_string()),
                        verdict,
                    })
                })
                .collect();
            let prunable = !standings.is_empty()
                && standings
                    .iter()
                    .all(|s| matches!(s.verdict, Verdict::CaughtUp { .. }));
            PackagePlan {
                name: pkg.name.clone(),
                prunable,
                branches: standings,
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The human-readable plan.
pub fn render(report: &Report) -> String {
    let mut out = String::new();
    let (prunable, in_flight): (Vec<&PackagePlan>, Vec<&PackagePlan>) =
        report.packages.iter().partition(|p| p.prunable);
    let branches: BTreeSet<&str> = report
        .packages
        .iter()
        .flat_map(|p| p.branches.iter().map(|s| s.branch.as_str()))
        .collect();
    let _ = writeln!(
        out,
        "COPR {}: {} package(s); target releases: {}\n",
        report.copr,
        report.packages.len(),
        branches.into_iter().collect::<Vec<_>>().join(", ")
    );
    if !prunable.is_empty() {
        let _ = writeln!(
            out,
            "Caught up everywhere ({}), safe to prune:",
            prunable.len()
        );
        for p in &prunable {
            let _ = writeln!(out, "  - {}: {}", p.name, standings_line(p));
        }
        let _ = writeln!(out);
    }
    if !in_flight.is_empty() {
        let _ = writeln!(out, "Still in flight ({}):", in_flight.len());
        for p in &in_flight {
            let _ = writeln!(out, "  - {}: {}", p.name, standings_line(p));
        }
        let _ = writeln!(out);
    }
    if !report.skipped_chroots.is_empty() {
        let _ = writeln!(
            out,
            "Skipped chroots (no branch known): {}",
            report.skipped_chroots.join(", ")
        );
    }
    out
}

fn standings_line(p: &PackagePlan) -> String {
    p.branches
        .iter()
        .map(|s| {
            let copr = s.copr_vr.as_deref().unwrap_or("?");
            match &s.verdict {
                Verdict::CaughtUp { branch_vr } => format!("{} {branch_vr} ≥ {copr}", s.branch),
                Verdict::Behind { branch_vr } => {
                    format!("{} has {branch_vr}, COPR {copr} (ahead)", s.branch)
                }
                Verdict::Absent => format!("{} absent, COPR {copr}", s.branch),
                Verdict::NotBuilt { state } => format!("{} not built ({state})", s.branch),
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Fetch the COPR, look each branch up, print the plan, and — on a
/// terminal, or with `--yes` — delete what every release has caught
/// up on.
pub fn run(opts: &Options) -> Result<(), String> {
    sandogasa_cli::require_tools(&[("fedrq", "sudo dnf install fedrq", Some("--version"))])?;
    let copr_label = format!("{}/{}", opts.owner, opts.project);
    let packages = sandogasa_copr::Copr::new()
        .monitor(&opts.owner, &opts.project)
        .map_err(|e| e.to_string())?;

    let mut branches: BTreeSet<String> = BTreeSet::new();
    let mut skipped: BTreeSet<String> = BTreeSet::new();
    for chroot in sandogasa_copr::available_chroots(&packages) {
        match chroot_branch(&chroot) {
            Some(b) => {
                branches.insert(b);
            }
            None => {
                skipped.insert(chroot);
            }
        }
    }
    let branches: Vec<String> = branches.into_iter().collect();
    let names: Vec<String> = packages.iter().map(|p| p.name.clone()).collect();

    // One fedrq query per target release, for every package at once.
    let mut versions: BranchVersions = BTreeMap::new();
    for branch in &branches {
        if opts.verbose {
            eprintln!(
                "[copr-prune] looking up {} package(s) on {branch}",
                names.len()
            );
        }
        let fedrq = sandogasa_fedrq::Fedrq {
            branch: Some(branch.clone()),
            repo: None,
        };
        for nvr in fedrq.src_nvrs(&names).map_err(|e| e.to_string())? {
            // `name-version-release`: the name is one of ours.
            if let Some(name) = names
                .iter()
                .filter(|n| nvr.starts_with(&format!("{n}-")))
                .max_by_key(|n| n.len())
            {
                versions.insert(
                    (branch.clone(), name.clone()),
                    nvr[name.len() + 1..].to_string(),
                );
            }
        }
    }

    let report = Report {
        copr: copr_label.clone(),
        packages: plan(&packages, &branches, &versions),
        skipped_chroots: skipped.into_iter().collect(),
    };
    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("JSON serialization failed")
        );
        return Ok(());
    }
    print!("{}", render(&report));

    let prunable: Vec<&PackagePlan> = report.packages.iter().filter(|p| p.prunable).collect();
    if prunable.is_empty() {
        return Ok(());
    }
    if !opts.yes && !opts.interactive {
        println!("Nothing deleted: pass --yes to prune, or run from a terminal to be asked.");
        return Ok(());
    }
    sandogasa_cli::require_tools(&[("copr-cli", "sudo dnf install copr-cli", Some("--version"))])?;
    let mut failures = 0;
    for p in prunable {
        if !opts.yes {
            let ok =
                sandogasa_cli::confirm(&format!("Delete {} from {copr_label}?", p.name), false)
                    .map_err(|e| e.to_string())?;
            if !ok {
                continue;
            }
        }
        match delete_package(&copr_label, &p.name) {
            Ok(()) => println!("deleted {}", p.name),
            Err(e) => {
                failures += 1;
                eprintln!("warning: {e}");
            }
        }
    }
    if failures > 0 {
        return Err(format!("{failures} deletion(s) failed"));
    }
    Ok(())
}

/// `copr-cli delete-package --name NAME owner/project`.
fn delete_package(copr: &str, name: &str) -> Result<(), String> {
    let out = std::process::Command::new("copr-cli")
        .args(["delete-package", "--name", name, copr])
        .output()
        .map_err(|e| format!("failed to run copr-cli: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "copr-cli delete-package {name} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandogasa_copr::ChrootState;

    fn package(name: &str, chroots: &[(&str, &str, &str)]) -> PackageStatus {
        PackageStatus {
            name: name.to_string(),
            chroots: chroots
                .iter()
                .map(|(c, state, vr)| {
                    (
                        c.to_string(),
                        ChrootState {
                            state: state.to_string(),
                            build_id: Some(1),
                            pkg_version: Some(vr.to_string()),
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn chroot_branch_inverts_chroot_prefix() {
        for (chroot, branch) in [
            ("fedora-rawhide-x86_64", "rawhide"),
            ("fedora-46-aarch64", "f46"),
            ("epel-9-x86_64", "epel9"),
            ("epel-10-ppc64le", "epel10"),
            ("centos-stream-10-x86_64", "c10s"),
            ("fedora-eln-x86_64", "eln"),
        ] {
            assert_eq!(chroot_branch(chroot).as_deref(), Some(branch), "{chroot}");
            let prefix = sandogasa_copr::chroot_prefix(branch).unwrap();
            assert!(chroot.starts_with(&prefix), "{chroot} vs {prefix}");
        }
        assert_eq!(chroot_branch("opensuse-leap-15.6-x86_64"), None);
        assert_eq!(chroot_branch("fedora-x86_64"), None);
    }

    #[test]
    fn plan_prunes_only_what_every_release_caught_up_on() {
        let packages = vec![
            // Landed in rawhide (a newer release even) and epel9.
            package(
                "rust-phf_shared",
                &[
                    ("fedora-rawhide-x86_64", "succeeded", "0.14.0-1"),
                    ("epel-9-x86_64", "succeeded", "0.14.0-1"),
                ],
            ),
            // rawhide caught up, epel9 has nothing yet.
            package(
                "rust-phf_macros",
                &[
                    ("fedora-rawhide-x86_64", "succeeded", "0.14.0-1"),
                    ("epel-9-x86_64", "succeeded", "0.14.0-1"),
                ],
            ),
            // rawhide still on the old release (COPR ahead).
            package(
                "rust-phf",
                &[("fedora-rawhide-x86_64", "succeeded", "0.14.0-1")],
            ),
            // Build failed: not caught up by definition.
            package(
                "rust-broken",
                &[("fedora-rawhide-x86_64", "failed", "1.0-1")],
            ),
        ];
        let branches = vec!["epel9".to_string(), "rawhide".to_string()];
        let versions: BranchVersions = [
            (("rawhide", "rust-phf_shared"), "0.14.0-2.fc46"),
            (("epel9", "rust-phf_shared"), "0.14.0-1.el9"),
            (("rawhide", "rust-phf_macros"), "0.14.0-1.fc46"),
            (("rawhide", "rust-phf"), "0.13.1-2.fc46"),
            (("rawhide", "rust-broken"), "1.0-1.fc46"),
        ]
        .into_iter()
        .map(|((b, n), v)| ((b.to_string(), n.to_string()), v.to_string()))
        .collect();
        let plan = plan(&packages, &branches, &versions);
        let by_name: BTreeMap<&str, &PackagePlan> =
            plan.iter().map(|p| (p.name.as_str(), p)).collect();
        assert!(by_name["rust-phf_shared"].prunable);
        assert!(!by_name["rust-phf_macros"].prunable);
        assert_eq!(
            by_name["rust-phf_macros"].branches[0].verdict,
            Verdict::Absent
        );
        assert_eq!(
            by_name["rust-phf"].branches[0].verdict,
            Verdict::Behind {
                branch_vr: "0.13.1-2.fc46".to_string()
            }
        );
        assert!(!by_name["rust-broken"].prunable);
        let text = render(&Report {
            copr: "@rust/uutils-and-nushell".to_string(),
            packages: plan,
            skipped_chroots: vec![],
        });
        assert!(text.contains("Caught up everywhere (1), safe to prune:\n  - rust-phf_shared: epel9 0.14.0-1.el9 ≥ 0.14.0-1; rawhide 0.14.0-2.fc46 ≥ 0.14.0-1"), "{text}");
        assert!(text.contains("Still in flight (3):"));
        assert!(text.contains("rust-phf: rawhide has 0.13.1-2.fc46, COPR 0.14.0-1 (ahead)"));
        assert!(text.contains("rust-broken: rawhide not built (failed)"));
    }
}
