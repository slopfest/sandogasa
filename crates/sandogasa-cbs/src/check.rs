// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What a SIG build changes for the packages that use it (#157): its
//! Provides compared with those of the build it replaces, and, for
//! each capability it drops, which packages in stock CentOS Stream and
//! EPEL require it at run time. The builds' Provides come from the CBS
//! hub, so a candidate can be checked before anyone can install it.
//!
//! Only dropped capabilities are looked up. A capability whose version
//! changes (`rasdaemon = 0.8.5` to `= 1.0.0`) is listed but not looked
//! up: nearly every package requiring it takes any version, and a
//! lookup would flag each routine update. A soname is dropped rather
//! than changed when it moves on (`libfoo.so.1` to `libfoo.so.2`), so
//! it is always looked up. A capability that another source package
//! in the same repos still provides is not a loss, and is shown as
//! such rather than looked up.

use std::collections::{BTreeMap, BTreeSet};

use sandogasa_depfilter::{ProvideChange, compare_provides, is_solib_dep, provide_name};
use sandogasa_kojihub::HubClient;

/// One build's check against the build it replaces.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub nvr: String,
    /// The build compared with, `None` when the collection has none
    /// in `-testing` or `-release` yet.
    pub against: Option<String>,
    /// The Provides the replaced build had that this one lacks as they
    /// were: dropped, or at another version.
    pub changes: Vec<ProvideChange>,
    /// Dropped capabilities that another source package still provides,
    /// as `(place, capability, providers)`: not a loss.
    pub still_provided: Vec<(String, String, Vec<String>)>,
    /// For each place looked in (`CentOS Stream 10`), the source
    /// packages there whose binaries require a dropped capability.
    pub users: Vec<(String, Vec<String>)>,
    /// Places that could not be looked in, with why.
    pub unchecked: Vec<String>,
}

impl Check {
    /// Whether a package elsewhere requires something this build drops.
    pub fn breaks(&self) -> bool {
        self.users.iter().any(|(_, users)| !users.is_empty())
    }

    /// Whether the build drops something whose users could not all be
    /// looked up: the check cannot say it breaks nothing.
    pub fn uncertain(&self) -> bool {
        !self.dropped().is_empty() && !self.unchecked.is_empty()
    }

    /// The Provides this build drops: no Provide of the name left.
    pub fn dropped(&self) -> Vec<&str> {
        self.changes
            .iter()
            .filter(|c| c.new.is_none())
            .map(|c| c.old.as_str())
            .collect()
    }

    /// The capabilities whose version changes, as `(old, new)`.
    pub fn changed(&self) -> Vec<(&str, &str)> {
        self.changes
            .iter()
            .filter_map(|c| c.new.as_deref().map(|n| (c.old.as_str(), n)))
            .collect()
    }

    /// The dropped capabilities, by name, sorted and without repeats.
    fn dropped_names(&self) -> Vec<String> {
        self.dropped()
            .into_iter()
            .map(|d| provide_name(d).to_string())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// Where to look for the packages using a build of EL `major`: stock
/// CentOS Stream and EPEL, as `(label, fedrq branch)`.
pub fn user_repos(major: u32) -> Vec<(String, String)> {
    vec![
        (format!("CentOS Stream {major}"), format!("c{major}s")),
        (format!("EPEL {major}"), format!("epel{major}")),
    ]
}

/// Whether `rpm` (`name-version-release.arch`) is a debuginfo or
/// debugsource package, whose Provides only debuggers use.
fn is_debug_rpm(rpm: &str) -> bool {
    let Some((rest, _arch)) = rpm.rsplit_once('.') else {
        return false;
    };
    // name-version-release: the name is what is left before the last
    // two dashes.
    let name = rest.rsplitn(3, '-').nth(2).unwrap_or(rest);
    name.ends_with("-debuginfo") || name.ends_with("-debugsource")
}

/// The Provides of each build in `nvrs`, as `name = version` strings,
/// from the binary RPMs the hub lists for it (debuginfo and debugsource
/// left out): two `multiCall`s for all the builds. One result per
/// build, in order; a build the hub cannot answer for is an error in its
/// own slot.
pub fn provides_of(
    hub: &HubClient,
    nvrs: &[String],
) -> Result<Vec<Result<BTreeSet<String>, String>>, String> {
    let rpms = hub.builds_rpm_nvras(nvrs).map_err(|e| e.to_string())?;
    let mut owners: Vec<usize> = Vec::new();
    let mut all: Vec<String> = Vec::new();
    let mut out: Vec<Result<BTreeSet<String>, String>> = Vec::new();
    for (i, listed) in rpms.into_iter().enumerate() {
        match listed {
            Ok(list) => {
                for rpm in list.into_iter().filter(|r| !is_debug_rpm(r)) {
                    owners.push(i);
                    all.push(rpm);
                }
                out.push(Ok(BTreeSet::new()));
            }
            Err(e) => out.push(Err(e.to_string())),
        }
    }
    let deps = hub.rpms_deps(&all, 1).map_err(|e| e.to_string())?;
    for (owner, (rpm, provides)) in owners.into_iter().zip(all.iter().zip(deps)) {
        match provides {
            Ok(list) => {
                if let Ok(set) = &mut out[owner] {
                    set.extend(list.into_iter().map(|(name, version)| match version {
                        Some(v) => format!("{name} = {v}"),
                        None => name,
                    }));
                }
            }
            Err(e) => out[owner] = Err(format!("{rpm}: {e}")),
        }
    }
    Ok(out)
}

/// `nvr`'s Provides `new` compared with `old`, those of the build it
/// replaces (`against`); nothing looked up yet ([`look_up`] does that).
pub fn compare(
    nvr: &str,
    against: Option<(&str, &BTreeSet<String>)>,
    new: &BTreeSet<String>,
) -> Check {
    let (against, changes) = match against {
        Some((a, old)) => (Some(a.to_string()), compare_provides(old, new)),
        None => (None, Vec::new()),
    };
    Check {
        nvr: nvr.to_string(),
        against,
        changes,
        still_provided: Vec::new(),
        users: Vec::new(),
        unchecked: Vec::new(),
    }
}

/// What [`look_up`] asks of the repos it looks in.
pub trait Lookup: Sync {
    /// The packages in fedrq `branch` providing any of `capabilities`,
    /// as `(source package, its Provides)`.
    fn providers(
        &self,
        branch: &str,
        capabilities: &[String],
    ) -> Result<Vec<(String, Vec<String>)>, String>;

    /// The source packages in `branch` whose binary packages require
    /// any of `capabilities` at run time.
    fn users(&self, branch: &str, capabilities: &[String]) -> Result<Vec<String>, String>;
}

/// What one place says about some dropped capabilities: still provided
/// by others, `(capability, providers)`, and the users of the rest.
type Answer = Result<(Vec<(String, Vec<String>)>, Vec<String>), String>;

/// One place's answer for `dropped`: which another source package (not
/// in `ours`) still provides, and the users of the rest.
fn ask(lookup: &dyn Lookup, branch: &str, dropped: &[String], ours: &[&str]) -> Answer {
    let providers = lookup.providers(branch, dropped)?;
    let mut still: Vec<(String, Vec<String>)> = Vec::new();
    let mut lost: Vec<String> = Vec::new();
    for cap in dropped {
        let others: BTreeSet<String> = providers
            .iter()
            .filter(|(source, provides)| {
                !ours.contains(&source.as_str()) && provides.iter().any(|p| provide_name(p) == cap)
            })
            .map(|(source, _)| source.clone())
            .collect();
        if others.is_empty() {
            lost.push(cap.clone());
        } else {
            still.push((cap.clone(), others.into_iter().collect()));
        }
    }
    let users = if lost.is_empty() {
        Vec::new()
    } else {
        lookup
            .users(branch, &lost)?
            .into_iter()
            .filter(|p| p != "(none)" && !ours.contains(&p.as_str()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    Ok((still, users))
}

/// Look up, for each check in `checks`, who uses what its build drops,
/// in the places `repos[i]` names for `checks[i]` ([`user_repos`]).
/// Packages in `ours` (the builds' own source package, and any promoted
/// with them) neither provide nor use. Each place is asked once per
/// distinct set of dropped capabilities, so variants of one release
/// that drop the same things share the queries, and the places are
/// asked concurrently.
pub fn look_up(
    checks: &mut [Check],
    repos: &[Vec<(String, String)>],
    ours: &[&str],
    lookup: &dyn Lookup,
) {
    // (label, branch, dropped) -> the checks asking it.
    let mut asks: BTreeMap<(String, String, Vec<String>), Vec<usize>> = BTreeMap::new();
    for (i, check) in checks.iter().enumerate() {
        let dropped = check.dropped_names();
        if dropped.is_empty() {
            continue;
        }
        for (label, branch) in repos.get(i).into_iter().flatten() {
            asks.entry((label.clone(), branch.clone(), dropped.clone()))
                .or_default()
                .push(i);
        }
    }
    let answers: Vec<Answer> = std::thread::scope(|s| {
        let handles: Vec<_> = asks
            .keys()
            .map(|(_, branch, dropped)| s.spawn(move || ask(lookup, branch, dropped, ours)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("the lookup panicked".into()))
            })
            .collect()
    });
    for (((label, _, _), indices), answer) in asks.iter().zip(answers) {
        for &i in indices {
            match &answer {
                Ok((still, users)) => {
                    checks[i].still_provided.extend(
                        still
                            .iter()
                            .map(|(cap, by)| (label.clone(), cap.clone(), by.clone())),
                    );
                    checks[i].users.push((label.clone(), users.clone()));
                }
                Err(e) => checks[i].unchecked.push(format!("{label}: {e}")),
            }
        }
    }
}

/// Whether `capability` is a shared library's soname, whose loss breaks
/// a binary built against it.
pub fn is_soname(capability: &str) -> bool {
    is_solib_dep(capability) && !sandogasa_depfilter::is_solib_symbol_dep(capability)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// Repos answering from fixed tables, recording what is asked.
    struct Fake {
        asked: Mutex<Vec<(String, String, Vec<String>)>>,
    }

    impl Lookup for Fake {
        fn providers(
            &self,
            branch: &str,
            caps: &[String],
        ) -> Result<Vec<(String, Vec<String>)>, String> {
            self.asked
                .lock()
                .unwrap()
                .push(("providers".into(), branch.into(), caps.to_vec()));
            match branch {
                "c10s" => Ok(vec![
                    // Ours, and stock's other package still shipping one.
                    ("foo".into(), vec!["foo-tools = 1".into()]),
                    ("compat".into(), vec!["libold.so.0()(64bit)".into()]),
                ]),
                _ => Err("no such release".into()),
            }
        }

        fn users(&self, branch: &str, caps: &[String]) -> Result<Vec<String>, String> {
            self.asked
                .lock()
                .unwrap()
                .push(("users".into(), branch.into(), caps.to_vec()));
            Ok(vec!["bar".into(), "foo".into(), "(none)".into()])
        }
    }

    #[test]
    fn a_dropped_capability_is_looked_up_and_a_version_change_is_not() {
        let old = set(&[
            "libfoo.so.1()(64bit)",
            "libold.so.0()(64bit)",
            "foo = 1.0-1.hs.el10",
            "foo-tools = 1.0-1.hs.el10",
        ]);
        let new = set(&["libfoo.so.2()(64bit)", "foo = 2.0-1.hs.el10"]);
        let mut checks = vec![
            compare("foo-2.0-1.hs.el10", Some(("foo-1.0-1.hs.el10", &old)), &new),
            // A variant dropping the same: the same queries serve it.
            compare(
                "foo-2.0-1.hs+fb.el10",
                Some(("foo-1.0-1.hs+fb.el10", &old)),
                &new,
            ),
        ];
        assert_eq!(
            checks[0].dropped(),
            [
                "foo-tools = 1.0-1.hs.el10",
                "libfoo.so.1()(64bit)",
                "libold.so.0()(64bit)"
            ]
        );
        assert_eq!(
            checks[0].changed(),
            [("foo = 1.0-1.hs.el10", "foo = 2.0-1.hs.el10")]
        );
        let fake = Fake {
            asked: Mutex::new(Vec::new()),
        };
        look_up(
            &mut checks,
            &[user_repos(10), user_repos(10)],
            &["foo"],
            &fake,
        );
        // Each place asked once for both builds; users only of what no
        // other package provides.
        let mut asked = fake.asked.lock().unwrap().clone();
        asked.sort();
        assert_eq!(asked.len(), 3, "{asked:?}");
        assert!(asked.contains(&(
            "users".into(),
            "c10s".into(),
            vec!["foo-tools".into(), "libfoo.so.1()(64bit)".into()]
        )));
        for c in &checks {
            assert_eq!(
                c.still_provided,
                [(
                    "CentOS Stream 10".to_string(),
                    "libold.so.0()(64bit)".to_string(),
                    vec!["compat".to_string()]
                )]
            );
            // Ours and fedrq's "(none)" are not users.
            assert_eq!(
                c.users,
                [("CentOS Stream 10".to_string(), vec!["bar".to_string()])]
            );
            assert!(c.breaks());
            assert_eq!(c.unchecked, ["EPEL 10: no such release"]);
            assert!(c.uncertain());
        }
    }

    #[test]
    fn nothing_dropped_asks_nothing_and_breaks_nothing() {
        struct Never;
        impl Lookup for Never {
            fn providers(
                &self,
                _: &str,
                _: &[String],
            ) -> Result<Vec<(String, Vec<String>)>, String> {
                panic!("nothing dropped, nothing to look up")
            }
            fn users(&self, _: &str, _: &[String]) -> Result<Vec<String>, String> {
                panic!("nothing dropped, nothing to look up")
            }
        }
        let old = set(&["foo = 1"]);
        let new = set(&["foo = 2", "foo-extra = 2"]);
        let mut checks = vec![
            compare("foo-2", Some(("foo-1", &old)), &new),
            // Nothing to compare with: nothing checked.
            compare("foo-3", None, &new),
        ];
        look_up(&mut checks, &[user_repos(9), user_repos(9)], &[], &Never);
        assert!(!checks[0].breaks() && !checks[0].uncertain());
        assert!(checks[0].dropped().is_empty());
        assert_eq!(checks[1].against, None);
        assert!(checks[1].changes.is_empty());
    }

    #[test]
    fn debug_packages_and_sonames_are_told_apart() {
        assert!(is_debug_rpm("rasdaemon-debuginfo-1.0.0-1.hs.el10.aarch64"));
        assert!(is_debug_rpm(
            "rasdaemon-debugsource-1.0.0-1.hs.el10.aarch64"
        ));
        assert!(!is_debug_rpm("rasdaemon-1.0.0-1.hs.el10.aarch64"));
        assert!(!is_debug_rpm("noarch"));
        assert!(is_soname("libfoo.so.1()(64bit)"));
        assert!(!is_soname("libc.so.6(GLIBC_2.38)(64bit)"));
        assert!(!is_soname("foo = 1"));
        assert_eq!(
            user_repos(9)[1],
            ("EPEL 9".to_string(), "epel9".to_string())
        );
    }

    /// A multiCall answer from `results`, each already a `<value>`.
    fn multicall(results: &[String]) -> String {
        format!(
            "<?xml version='1.0'?><methodResponse><params><param>\
             <value><array><data>{}</data></array></value>\
             </param></params></methodResponse>",
            results.concat()
        )
    }

    /// One call's answer in a multiCall: a one-element array.
    fn ok(value: &str) -> String {
        format!("<value><array><data>{value}</data></array></value>")
    }

    fn rpm(name: &str, arch: &str) -> String {
        let f = |k: &str, v: &str| {
            format!("<member><name>{k}</name><value><string>{v}</string></value></member>")
        };
        format!(
            "<value><struct>{}{}{}{}</struct></value>",
            f("name", name),
            f("version", "1.0.0"),
            f("release", "1.hs.el10"),
            f("arch", arch)
        )
    }

    fn provide(name: &str, version: &str) -> String {
        format!(
            "<value><struct><member><name>name</name><value><string>{name}</string></value></member>\
             <member><name>version</name><value><string>{version}</string></value></member>\
             <member><name>type</name><value><int>1</int></value></member></struct></value>"
        )
    }

    #[test]
    fn provides_are_read_from_the_hub_in_two_calls() {
        let mut server = mockito::Server::new();
        let fault = "<value><struct>\
             <member><name>faultCode</name><value><int>1000</int></value></member>\
             <member><name>faultString</name><value><string>No such build</string></value></member>\
             </struct></value>";
        let rpms = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("listBuildRPMs".into()))
            .with_body(multicall(&[
                ok(&format!(
                    "<value><array><data>{}{}{}</data></array></value>",
                    rpm("rasdaemon", "aarch64"),
                    rpm("rasdaemon-debuginfo", "aarch64"),
                    rpm("rasdaemon", "src")
                )),
                fault.to_string(),
            ]))
            .expect(1)
            .create();
        let deps = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("getRPMDeps".into()))
            .with_body(multicall(&[ok(&format!(
                "<value><array><data>{}{}</data></array></value>",
                provide("rasdaemon", "1.0.0-1.hs.el10"),
                provide("config(rasdaemon)", "")
            ))]))
            .expect(1)
            .create();
        let hub = HubClient::new(&server.url());
        let got = provides_of(
            &hub,
            &["rasdaemon-1.0.0-1.hs.el10".into(), "gone-1-1".into()],
        )
        .unwrap();
        rpms.assert();
        deps.assert();
        assert_eq!(
            got[0],
            Ok(set(&["config(rasdaemon)", "rasdaemon = 1.0.0-1.hs.el10"]))
        );
        assert!(got[1].is_err());
        // A hub that cannot be reached is an error, not "no Provides".
        assert!(provides_of(&HubClient::new("http://127.0.0.1:1"), &["x-1-1".into()]).is_err());
    }
}
