// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The versions Fedora and CentOS Stream carry, read from their own
//! repositories with fedrq rather than from Repology (#209).
//!
//! Repology answers this network with a bot check since 2026-11-20, and
//! a refusal used to leave the Fedora and CentOS Stream columns empty
//! and every package tracking them without a version to compare with.
//! fedrq asks the distributions directly, for every package of a run in
//! one query per branch: rawhide, the newest Fedora release (branched
//! included, as Repology's `fedora_NN` is), and the newest CentOS Stream.

use std::collections::HashMap;

/// A package's version in each of a run's distributions, by source
/// package name. A field is `None` when that branch was not read
/// (fedrq missing or failing), which leaves the column to Repology; a
/// branch read without a package means the package is not there.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct DistroVersions {
    pub fedora_rawhide: Option<HashMap<String, String>>,
    /// The newest Fedora release below rawhide, and its versions.
    pub fedora_stable: Option<(u32, HashMap<String, String>)>,
    /// The newest CentOS Stream release, and its versions.
    pub centos_stream: Option<(u32, HashMap<String, String>)>,
}

/// What to read: the columns the run shows, and the ones its packages
/// are tracked against.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Wanted {
    pub fedora_rawhide: bool,
    pub fedora_stable: bool,
    pub centos_stream: bool,
}

/// The NVRs of `packages`' source packages on a branch: one query for
/// all of them ([`sandogasa_fedrq::Fedrq::src_nvrs`]), or a stand-in.
pub type SrcNvrs<'a> = &'a dyn Fn(&str, &[String]) -> Result<Vec<String>, String>;

/// Read what `wanted` asks for, for `packages`, with `query`; the
/// newest CentOS Stream is `stream_major`. A branch that cannot be read
/// is left out with a warning, and its column is Repology's.
pub fn read(
    packages: &[String],
    wanted: Wanted,
    stream_major: u32,
    query: SrcNvrs,
) -> DistroVersions {
    let mut out = DistroVersions::default();
    if packages.is_empty() || wanted == Wanted::default() {
        return out;
    }
    eprintln!("Reading Fedora and CentOS Stream versions with fedrq…");
    let read_branch = |branch: &str| match query(branch, packages) {
        Ok(nvrs) => Some(versions(&nvrs)),
        Err(e) => {
            eprintln!("warning: fedrq cannot read {branch} ({e}); asking Repology instead");
            None
        }
    };
    if wanted.fedora_rawhide {
        out.fedora_rawhide = read_branch("rawhide");
    }
    if wanted.fedora_stable {
        // Rawhide's fedora-release is the next release's number.
        let rawhide = query("rawhide", &[RELEASE_PACKAGE.to_string()])
            .map_err(|e| e.to_string())
            .and_then(|nvrs| {
                versions(&nvrs)
                    .get(RELEASE_PACKAGE)
                    .and_then(|v| v.parse::<u32>().ok())
                    .ok_or_else(|| format!("no {RELEASE_PACKAGE} in rawhide"))
            });
        match rawhide {
            Ok(n) if n > 1 => {
                let stable = n - 1;
                out.fedora_stable = read_branch(&format!("f{stable}")).map(|v| (stable, v));
            }
            Ok(n) => eprintln!(
                "warning: rawhide's {RELEASE_PACKAGE} is version {n}, which names no release \
                 below it; asking Repology instead"
            ),
            Err(e) => eprintln!(
                "warning: cannot tell the newest Fedora release with fedrq ({e}); asking \
                 Repology instead"
            ),
        }
    }
    if wanted.centos_stream {
        out.centos_stream = read_branch(&format!("c{stream_major}s")).map(|v| (stream_major, v));
    }
    out
}

/// [`read`] with fedrq itself, when it is installed: without it every
/// column stays Repology's, which is said once.
pub fn read_with_fedrq(packages: &[String], wanted: Wanted, stream_major: u32) -> DistroVersions {
    if wanted == Wanted::default() {
        return DistroVersions::default();
    }
    if sandogasa_cli::require_tools(&[("fedrq", "sudo dnf install fedrq", None)]).is_err() {
        eprintln!(
            "note: fedrq is not installed, so Fedora and CentOS Stream versions come from \
             Repology (sudo dnf install fedrq)"
        );
        return DistroVersions::default();
    }
    let query = |branch: &str, names: &[String]| {
        sandogasa_fedrq::Fedrq::for_branch(branch)
            .src_nvrs(names)
            .map_err(|e| e.to_string())
    };
    read(packages, wanted, stream_major, &query)
}

/// The source package whose version names a Fedora release.
const RELEASE_PACKAGE: &str = "fedora-release";

/// Split an NVR into its name and version.
fn name_version(nvr: &str) -> Option<(&str, &str)> {
    let mut parts = nvr.rsplitn(3, '-');
    let (_release, version, name) = (parts.next()?, parts.next()?, parts.next()?);
    Some((name, version))
}

/// The newest version of each package among `nvrs`.
fn versions(nvrs: &[String]) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    for (name, version) in nvrs.iter().filter_map(|n| name_version(n)) {
        let newer = out.get(name).is_none_or(|v| {
            sandogasa_rpmvercmp::rpmvercmp(version, v) == std::cmp::Ordering::Greater
        });
        if newer {
            out.insert(name.to_string(), version.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn each_branch_is_read_once_for_every_package() {
        let asked = std::cell::RefCell::new(Vec::new());
        let query = |branch: &str, names: &[String]| {
            asked.borrow_mut().push((branch.to_string(), names.len()));
            Ok(match branch {
                "rawhide" if names == [RELEASE_PACKAGE] => s(&["fedora-release-46-0.1"]),
                "rawhide" => s(&["kexec-tools-2.0.33-1.fc46", "rust-foo-bar-1.2-1.fc46"]),
                "f45" => s(&["kexec-tools-2.0.32-3.fc45", "kexec-tools-2.0.32-5.fc45"]),
                "c10s" => s(&["kexec-tools-2.0.32-3.el10"]),
                _ => Vec::new(),
            })
        };
        let all = Wanted {
            fedora_rawhide: true,
            fedora_stable: true,
            centos_stream: true,
        };
        let v = read(
            &s(&["kexec-tools", "rust-foo-bar", "gone"]),
            all,
            10,
            &query,
        );
        let rawhide = v.fedora_rawhide.unwrap();
        assert_eq!(rawhide["kexec-tools"], "2.0.33");
        // A name with dashes keeps them.
        assert_eq!(rawhide["rust-foo-bar"], "1.2");
        assert!(!rawhide.contains_key("gone"));
        let (n, stable) = v.fedora_stable.unwrap();
        assert_eq!(n, 45);
        assert_eq!(stable["kexec-tools"], "2.0.32");
        assert_eq!(v.centos_stream.unwrap().0, 10);
        // One query per branch, and one for the release number.
        assert_eq!(
            *asked.borrow(),
            [
                ("rawhide".to_string(), 3),
                ("rawhide".to_string(), 1),
                ("f45".to_string(), 3),
                ("c10s".to_string(), 3)
            ]
        );
    }

    #[test]
    fn a_branch_fedrq_cannot_read_is_left_to_repology() {
        let query = |branch: &str, _: &[String]| match branch {
            "c10s" => Ok(s(&["foo-1-1.el10"])),
            _ => Err("no metadata".to_string()),
        };
        let all = Wanted {
            fedora_rawhide: true,
            fedora_stable: true,
            centos_stream: true,
        };
        let v = read(&s(&["foo"]), all, 10, &query);
        assert_eq!(v.fedora_rawhide, None);
        assert_eq!(v.fedora_stable, None);
        assert_eq!(v.centos_stream.unwrap().1["foo"], "1");
        // Nothing wanted, or no packages: nothing asked.
        let never = |_: &str, _: &[String]| -> Result<Vec<String>, String> {
            panic!("asked fedrq for nothing")
        };
        assert_eq!(
            read(&s(&["foo"]), Wanted::default(), 10, &never),
            DistroVersions::default()
        );
        assert_eq!(read(&[], all, 10, &never), DistroVersions::default());
        assert_eq!(
            read_with_fedrq(&s(&["foo"]), Wanted::default(), 10),
            DistroVersions::default()
        );
    }

    #[test]
    fn a_rawhide_without_a_release_number_leaves_stable_to_repology() {
        let query = |_: &str, names: &[String]| {
            Ok(if names == [RELEASE_PACKAGE] {
                Vec::new()
            } else {
                s(&["foo-1-1.fc46"])
            })
        };
        let wanted = Wanted {
            fedora_stable: true,
            ..Wanted::default()
        };
        assert_eq!(read(&s(&["foo"]), wanted, 10, &query).fedora_stable, None);
        // A release number with no release below it is not read either.
        let query = |_: &str, names: &[String]| {
            Ok(if names == [RELEASE_PACKAGE] {
                s(&["fedora-release-1-1"])
            } else {
                s(&["foo-1-1.fc1"])
            })
        };
        assert_eq!(read(&s(&["foo"]), wanted, 10, &query).fedora_stable, None);
    }
}
