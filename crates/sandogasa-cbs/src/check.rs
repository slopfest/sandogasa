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

/// A dropped capability and the source packages requiring it, in one
/// place.
pub type Required = (String, Vec<String>);

/// One build's check against the build it replaces.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub nvr: String,
    /// The build compared with, `None` when the collection has none
    /// in `-release` yet.
    pub against: Option<String>,
    /// The Provides the replaced build had that this one lacks as they
    /// were: dropped, or at another version.
    pub changes: Vec<ProvideChange>,
    /// Dropped capabilities that another source package still provides,
    /// as `(place, capability, providers)`: not a loss.
    pub still_provided: Vec<(String, String, Vec<String>)>,
    /// For each place looked in (`CentOS Stream 10`), each dropped
    /// capability that source packages there require at run time, with
    /// those packages; empty when nothing there requires any. A
    /// capability left empty (`""`) holds users whose Requires named
    /// none of them plainly (a rich dependency).
    pub users: Vec<(String, Vec<Required>)>,
    /// Places that could not be looked in, with why.
    pub unchecked: Vec<String>,
}

impl Check {
    /// Whether a package elsewhere requires something this build drops.
    pub fn breaks(&self) -> bool {
        self.users.iter().any(|(_, required)| !required.is_empty())
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

    /// The whole check, as it shows at the terminal, in Markdown for a
    /// tracking-issue comment: what was compared, what the build drops,
    /// which of that another package still provides or packages
    /// elsewhere require, where could not be looked, and every version
    /// change, the last in a collapsed `<details>`. Each part is its own
    /// paragraph, as GitLab joins single lines.
    pub fn markdown(&self) -> String {
        let Some(against) = &self.against else {
            return "Checked what it drops: nothing in `-release` to compare it with yet."
                .to_string();
        };
        let dropped: BTreeSet<&str> = self.dropped().into_iter().collect();
        let mut out =
            format!("Checked what it drops against `{against}`, the newest in `-release`:");
        if dropped.is_empty() {
            out.push_str("\n\n- drops no Provides");
        } else {
            out.push('\n');
            for d in &dropped {
                out.push_str(&format!("\n- drops `{d}`"));
            }
        }
        for (place, cap, by) in &self.still_provided {
            out.push_str(&format!(
                "\n\n`{cap}` is still provided in {place} by {}.",
                by.join(", ")
            ));
        }
        for (place, cap, users) in self.required() {
            let what = if cap.is_empty() {
                "Something it drops".to_string()
            } else {
                format!("`{cap}`")
            };
            out.push_str(&format!(
                "\n\n**{what} is required in {place} by {}.**",
                users.join(", ")
            ));
        }
        // Only once a lookup ran: no place looked in proves nothing.
        if !dropped.is_empty()
            && !self.users.is_empty()
            && !self.breaks()
            && self.unchecked.is_empty()
        {
            out.push_str("\n\nNothing in CentOS Stream or EPEL requires what it drops.");
        }
        if !self.unchecked.is_empty() {
            let places: Vec<String> = self
                .unchecked
                .iter()
                .map(|u| format!("`{}`", one_line(u)))
                .collect();
            out.push_str(&format!("\n\nNot looked in: {}.", places.join(", ")));
        }
        let changed = self.changed();
        if !changed.is_empty() {
            let n = changed.len();
            out.push_str(&format!(
                "\n\n<details><summary>Changes {n} Provide{} to another version</summary>\n",
                if n == 1 { "" } else { "s" }
            ));
            for (old, new) in changed {
                out.push_str(&format!("\n- `{old}` to `{new}`"));
            }
            out.push_str("\n\n</details>");
        }
        out
    }

    /// Who requires what the build drops, as `(place, capability,
    /// users)`, one row per capability someone requires; an empty
    /// capability for users whose Requires named none plainly.
    pub fn required(&self) -> Vec<(&str, &str, &[String])> {
        self.users
            .iter()
            .flat_map(|(place, required)| {
                required
                    .iter()
                    .map(move |(cap, users)| (place.as_str(), cap.as_str(), users.as_slice()))
            })
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

/// `text` (an error from fedrq or the hub, a command line) on one
/// line, for a code span in a comment: newlines and runs of space
/// become one space, and a backtick a quote, so it cannot end the span
/// or start a paragraph.
pub fn one_line(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('`', "'")
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

    /// The binary packages in `branch` requiring any of `capabilities`
    /// at run time, as `(source package, the binary's Requires)`.
    fn users(
        &self,
        branch: &str,
        capabilities: &[String],
    ) -> Result<Vec<(String, Vec<String>)>, String>;
}

/// What one place says about some dropped capabilities: still provided
/// by others, `(capability, providers)`, and per capability lost, its
/// users.
type Answer = Result<(Vec<(String, Vec<String>)>, Vec<(String, Vec<String>)>), String>;

/// Whether the dependency `requirement` names `capability`: by name,
/// any version, or as a whole term of a rich dependency
/// (`(foo or bar)`), never as part of a longer name. A term keeps the
/// parentheses of its own name (`libfoo.so.1()(64bit)`, `perl(Foo)`)
/// and loses only those that group it.
fn names(requirement: &str, capability: &str) -> bool {
    requirement
        .split_whitespace()
        .any(|term| ungrouped(term) == capability)
}

/// `term` without the grouping parentheses of a rich dependency around
/// it: a leading `(` or a trailing `)` that the term itself does not
/// close or open.
fn ungrouped(mut term: &str) -> &str {
    let depth = |t: &str| t.matches('(').count() as i64 - t.matches(')').count() as i64;
    while term.starts_with('(') && depth(term) > 0 {
        term = &term[1..];
    }
    while term.ends_with(')') && depth(term) < 0 {
        term = &term[..term.len() - 1];
    }
    term
}

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
    let mut by_cap: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    if !lost.is_empty() {
        for (source, requires) in lookup.users(branch, &lost)? {
            if source == "(none)" || ours.contains(&source.as_str()) {
                continue;
            }
            let caps: Vec<&String> = lost
                .iter()
                .filter(|cap| requires.iter().any(|r| names(r, cap)))
                .collect();
            if caps.is_empty() {
                by_cap
                    .entry(String::new())
                    .or_default()
                    .insert(source.clone());
            }
            for cap in caps {
                by_cap
                    .entry(cap.clone())
                    .or_default()
                    .insert(source.clone());
            }
        }
    }
    let users = by_cap
        .into_iter()
        .map(|(cap, users)| (cap, users.into_iter().collect()))
        .collect();
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

        fn users(
            &self,
            branch: &str,
            caps: &[String],
        ) -> Result<Vec<(String, Vec<String>)>, String> {
            self.asked
                .lock()
                .unwrap()
                .push(("users".into(), branch.into(), caps.to_vec()));
            let req = |r: &[&str]| r.iter().map(|s| s.to_string()).collect();
            Ok(vec![
                ("bar".into(), req(&["libfoo.so.1()(64bit)", "glibc"])),
                ("baz".into(), req(&["foo-tools >= 1.0"])),
                ("qux".into(), req(&["(foo-tools or other)"])),
                ("odd".into(), req(&["something else"])),
                ("foo".into(), req(&["libfoo.so.1()(64bit)"])),
                ("(none)".into(), req(&["libfoo.so.1()(64bit)"])),
            ])
        }
    }

    #[test]
    fn a_requirement_names_a_capability_as_a_whole_term() {
        assert!(names("foo", "foo"));
        assert!(names("foo >= 1.0", "foo"));
        assert!(names("(foo-tools or other)", "foo-tools"));
        assert!(!names("(foo-tools or other)", "foo"));
        assert!(names("libfoo.so.1()(64bit)", "libfoo.so.1()(64bit)"));
        assert!(names(
            "(libfoo.so.1()(64bit) if bar)",
            "libfoo.so.1()(64bit)"
        ));
        assert!(names("((perl(Foo) >= 2) or baz)", "perl(Foo)"));
        assert!(!names("(perl(Foo::Bar) or baz)", "perl(Foo)"));
        assert!(!names("foo-tools", "foo"));
    }

    #[test]
    fn a_check_reads_as_a_comment_in_full() {
        let old = set(&["libr.so.1()(64bit)", "r-tools = 1-1", "r = 1-1"]);
        let mut c = compare("r-2-1", Some(("r-1-1", &old)), &set(&["r = 2-1"]));
        c.still_provided = vec![(
            "CentOS Stream 10".into(),
            "r-tools".into(),
            vec!["other".into()],
        )];
        c.users = vec![
            (
                "CentOS Stream 10".into(),
                vec![
                    ("".into(), vec!["c".into()]),
                    ("libr.so.1()(64bit)".into(), vec!["a".into(), "b".into()]),
                ],
            ),
            ("EPEL 10".into(), vec![]),
        ];
        assert_eq!(
            c.markdown(),
            "Checked what it drops against `r-1-1`, the newest in `-release`:\n\n\
             - drops `libr.so.1()(64bit)`\n\
             - drops `r-tools = 1-1`\n\n\
             `r-tools` is still provided in CentOS Stream 10 by other.\n\n\
             **Something it drops is required in CentOS Stream 10 by c.**\n\n\
             **`libr.so.1()(64bit)` is required in CentOS Stream 10 by a, b.**\n\n\
             <details><summary>Changes 1 Provide to another version</summary>\n\n\
             - `r = 1-1` to `r = 2-1`\n\n\
             </details>"
        );
        // Looked up everywhere, needed nowhere.
        c.users = vec![("CentOS Stream 10".into(), vec![])];
        c.still_provided.clear();
        assert!(
            c.markdown()
                .contains("\n\nNothing in CentOS Stream or EPEL requires what it drops.\n\n")
        );
        // Not looked up at all: no such claim.
        c.users.clear();
        assert!(!c.markdown().contains("Nothing in"), "{}", c.markdown());
        // A place that could not be looked in, its error on one line.
        c.unchecked = vec!["EPEL 10: no such\n`release`".into()];
        assert!(
            c.markdown()
                .contains("\n\nNot looked in: `EPEL 10: no such 'release'`.\n\n"),
            "{}",
            c.markdown()
        );
        // Nothing dropped, two versions changed.
        let same = compare(
            "r-2-1",
            Some(("r-1-1", &set(&["r = 1-1", "r-data = 1-1"]))),
            &set(&["r = 2-1", "r-data = 2-1"]),
        );
        assert_eq!(
            same.markdown(),
            "Checked what it drops against `r-1-1`, the newest in `-release`:\n\n\
             - drops no Provides\n\n\
             <details><summary>Changes 2 Provides to another version</summary>\n\n\
             - `r = 1-1` to `r = 2-1`\n\
             - `r-data = 1-1` to `r-data = 2-1`\n\n\
             </details>"
        );
        let first = compare("r-1-1", None, &set(&["r = 1-1"]));
        assert_eq!(
            first.markdown(),
            "Checked what it drops: nothing in `-release` to compare it with yet."
        );
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
            // Ours and fedrq's "(none)" are not users; each user goes
            // under what it requires, by name, versioned or in a rich
            // dependency, and one naming none plainly under "".
            let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            assert_eq!(
                c.users,
                [(
                    "CentOS Stream 10".to_string(),
                    vec![
                        (String::new(), s(&["odd"])),
                        ("foo-tools".to_string(), s(&["baz", "qux"])),
                        ("libfoo.so.1()(64bit)".to_string(), s(&["bar"])),
                    ]
                )]
            );
            assert_eq!(c.required().len(), 3);
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
            fn users(&self, _: &str, _: &[String]) -> Result<Vec<(String, Vec<String>)>, String> {
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
