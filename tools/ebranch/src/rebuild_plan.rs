// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Plan the rebuilds a library's SONAME bump calls for: which packages
//! link the library and so must be rebuilt against the new one, which
//! of them a side tag already has, and which cannot be rebuilt at all
//! because their BuildRequires were already broken before the bump.
//!
//! The dual of `resolve`: that one asks what a package needs to get
//! into a release, this one asks what has to follow a library that
//! changed underneath its dependents.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use sandogasa_fedrq::PkgInfo;
use serde::Serialize;

use crate::check_update::{KojiCli, KojiQuery};

/// What to plan and how to print it.
pub struct Options {
    /// The library's source package (`libical`).
    pub library: String,
    /// The branch the library is being bumped on (`rawhide`).
    pub branch: String,
    /// The side tag the rebuilds go into, to say which are done.
    pub side_tag: Option<String>,
    pub json: bool,
    pub verbose: bool,
}

/// How a dependent uses the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Link {
    /// A binary package requires one of the library's SONAMEs: it
    /// fails to install against the new library until rebuilt.
    Runtime,
    /// Only the source package BuildRequires the library's development
    /// files; nothing it ships links the library.
    BuildOnly,
}

/// One package that uses the library.
#[derive(Debug, Clone, Serialize)]
pub struct Dependent {
    pub package: String,
    pub link: Link,
    /// The build in the side tag, once the package has been rebuilt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rebuilt: Option<String>,
}

/// A dependent that cannot be rebuilt: some of its BuildRequires do
/// not resolve on the branch even before the bump.
#[derive(Debug, Clone, Serialize)]
pub struct Unbuildable {
    pub package: String,
    /// The BuildRequires that resolve to nothing.
    pub unresolved: Vec<String>,
    /// Packages requiring it at runtime, which become uninstallable
    /// along with it once the new library lands.
    pub stranded: Vec<String>,
}

/// The whole plan, as `--json` prints it.
#[derive(Debug, Clone, Serialize)]
pub struct RebuildPlan {
    pub library: String,
    pub branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side_tag: Option<String>,
    /// The library's SONAMEs on the branch, which the bump removes.
    pub sonames: Vec<String>,
    pub dependents: Vec<Dependent>,
    pub unbuildable: Vec<Unbuildable>,
    /// Packages rebuilt in the side tag that do not use the library.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rebuilt_not_needed: Vec<String>,
}

impl RebuildPlan {
    fn is_unbuildable(&self, package: &str) -> bool {
        self.unbuildable.iter().any(|u| u.package == package)
    }

    /// Runtime dependents not yet rebuilt and able to be.
    pub fn pending(&self) -> Vec<&Dependent> {
        self.dependents
            .iter()
            .filter(|d| d.link == Link::Runtime && d.rebuilt.is_none())
            .filter(|d| !self.is_unbuildable(&d.package))
            .collect()
    }

    /// Dependents the side tag already has, whatever their link.
    pub fn rebuilt(&self) -> Vec<&Dependent> {
        self.dependents
            .iter()
            .filter(|d| d.rebuilt.is_some())
            .collect()
    }

    /// Build-only dependents not rebuilt.
    pub fn build_only(&self) -> Vec<&Dependent> {
        self.dependents
            .iter()
            .filter(|d| d.link == Link::BuildOnly && d.rebuilt.is_none())
            .collect()
    }
}

/// What the plan asks the package repositories.
///
/// Behind a trait so a test answers from recorded data; a run asks
/// fedrq.
pub(crate) trait PlanQuery: Sync {
    /// Everything the source package's binaries provide.
    fn subpkgs_provides(&self, srpm: &str) -> Result<Vec<String>, String>;

    /// The binary package names a source package builds.
    fn subpkgs_names(&self, srpm: &str) -> Result<Vec<String>, String>;

    /// Source packages whose binaries require any of the capabilities.
    fn whatrequires(&self, capabilities: &[String]) -> Result<Vec<String>, String>;

    /// Source packages whose BuildRequires name any of the capabilities.
    fn whatrequires_src(&self, capabilities: &[String]) -> Result<Vec<String>, String>;

    /// A source package's BuildRequires.
    fn src_requires(&self, srpm: &str) -> Result<Vec<String>, String>;

    /// The binary packages providing any of the dependencies.
    fn providers(&self, deps: &[String]) -> Result<Vec<PkgInfo>, String>;
}

/// Work out the plan for `library` on the branch `q` reads, with the
/// side tag's builds when there is one.
pub(crate) fn plan(
    library: &str,
    branch: &str,
    side_tag: Option<(&str, &[String])>,
    q: &dyn PlanQuery,
    verbose: bool,
) -> Result<RebuildPlan, String> {
    let provides = q.subpkgs_provides(library)?;
    if provides.is_empty() {
        return Err(format!("{library} has no binary packages on {branch}"));
    }
    let (sonames, build_caps) = split_provides(&provides);
    if sonames.is_empty() {
        return Err(format!(
            "{library} provides no shared library on {branch}, so there is no SONAME to bump"
        ));
    }
    if verbose {
        eprintln!(
            "[rebuild-plan] {library}: {} SONAME(s), {} build capability(ies)",
            sonames.len(),
            build_caps.len()
        );
    }

    let runtime: BTreeSet<String> = q
        .whatrequires(&sonames)?
        .into_iter()
        .filter(|p| p != library)
        .collect();
    let build: BTreeSet<String> = q
        .whatrequires_src(&build_caps)?
        .into_iter()
        .filter(|p| p != library)
        .collect();
    if verbose {
        eprintln!(
            "[rebuild-plan] {} runtime dependent(s), {} building against it",
            runtime.len(),
            build.len()
        );
    }

    let tagged: BTreeMap<String, String> = side_tag
        .map(|(_, nvrs)| {
            nvrs.iter()
                .filter_map(|nvr| {
                    sandogasa_koji::parse_nvr_name(nvr).map(|n| (n.to_string(), nvr.clone()))
                })
                .collect()
        })
        .unwrap_or_default();

    let dependents: Vec<Dependent> = runtime
        .union(&build)
        .map(|p| Dependent {
            package: p.clone(),
            link: if runtime.contains(p) {
                Link::Runtime
            } else {
                Link::BuildOnly
            },
            rebuilt: tagged.get(p).cloned(),
        })
        .collect();

    let rebuilt_not_needed: Vec<String> = tagged
        .keys()
        .filter(|p| *p != library && !runtime.contains(*p) && !build.contains(*p))
        .cloned()
        .collect();

    let to_check: Vec<&str> = dependents
        .iter()
        .filter(|d| d.link == Link::Runtime && d.rebuilt.is_none())
        .map(|d| d.package.as_str())
        .collect();
    let unbuildable = find_unbuildable(&to_check, q, verbose)?;

    Ok(RebuildPlan {
        library: library.to_string(),
        branch: branch.to_string(),
        side_tag: side_tag.map(|(t, _)| t.to_string()),
        sonames,
        dependents,
        unbuildable,
        rebuilt_not_needed,
    })
}

/// The plan as Markdown, for a mailing list post or a Bodhi comment.
pub fn render(plan: &RebuildPlan) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Rebuild plan for {} ({})\n",
        plan.library, plan.branch
    );
    if let Some(tag) = &plan.side_tag {
        let _ = writeln!(out, "Side tag: `{tag}`\n");
    }
    let sonames: Vec<String> = plan.sonames.iter().map(|s| format!("`{s}`")).collect();
    let _ = writeln!(out, "SONAMEs on {}: {}\n", plan.branch, sonames.join(", "));

    let pending = plan.pending();
    let heading = if plan.side_tag.is_some() {
        "Still to rebuild"
    } else {
        "Needs a rebuild"
    };
    let _ = writeln!(out, "## {heading} ({})\n", pending.len());
    if pending.is_empty() {
        let _ = writeln!(out, "Nothing.\n");
    } else {
        for d in &pending {
            let _ = writeln!(out, "- {}", d.package);
        }
        out.push('\n');
    }

    if !plan.unbuildable.is_empty() {
        let _ = writeln!(out, "## Cannot be rebuilt ({})\n", plan.unbuildable.len());
        let _ = writeln!(
            out,
            "BuildRequires that already fail to resolve on {}, independently of {}:\n",
            plan.branch, plan.library
        );
        for u in &plan.unbuildable {
            let deps: Vec<String> = u.unresolved.iter().map(|d| format!("`{d}`")).collect();
            let _ = write!(out, "- {}: {}", u.package, deps.join(", "));
            if !u.stranded.is_empty() {
                let _ = write!(
                    out,
                    ". Once the new {} lands, these become uninstallable with it: {}",
                    plan.library,
                    u.stranded.join(", ")
                );
            }
            out.push('\n');
        }
        out.push('\n');
    }

    if let Some(tag) = &plan.side_tag {
        let rebuilt = plan.rebuilt();
        let _ = writeln!(out, "## Already rebuilt in {tag} ({})\n", rebuilt.len());
        if rebuilt.is_empty() {
            let _ = writeln!(out, "Nothing yet.\n");
        } else {
            for d in &rebuilt {
                let nvr = d.rebuilt.as_deref().unwrap_or_default();
                let _ = writeln!(out, "- {} (`{nvr}`)", d.package);
            }
            out.push('\n');
        }
    }

    let build_only = plan.build_only();
    if !build_only.is_empty() {
        let _ = writeln!(out, "## Build-time only ({})\n", build_only.len());
        let _ = writeln!(
            out,
            "These BuildRequire {}'s development files but ship nothing that links it. \
             They need a rebuild only if what they build depends on its version.\n",
            plan.library
        );
        for d in &build_only {
            let _ = writeln!(out, "- {}", d.package);
        }
        out.push('\n');
    }

    if !plan.rebuilt_not_needed.is_empty() {
        let _ = writeln!(
            out,
            "## Rebuilt but not using {} ({})\n",
            plan.library,
            plan.rebuilt_not_needed.len()
        );
        let _ = writeln!(
            out,
            "In the side tag, but neither linking nor building against {}. \
             Harmless, unless a rebuild changed a SONAME of its own.\n",
            plan.library
        );
        for p in &plan.rebuilt_not_needed {
            let _ = writeln!(out, "- {p}");
        }
        out.push('\n');
    }

    let _ = writeln!(
        out,
        "Packages further down a dependency chain need a rebuild only if a rebuilt \
         package changes a SONAME of its own. `ebranch check-update <side-tag>` shows \
         whether any did."
    );
    out
}

/// Plan the rebuilds and print the result.
pub fn run(opts: &Options) -> Result<(), String> {
    sandogasa_cli::require_tools(&[("fedrq", "sudo dnf install fedrq", Some("--version"))])?;
    let tagged = match &opts.side_tag {
        Some(tag) => {
            // `koji` has no `--version`, and `koji version` asks the hub, so
            // only its presence is checked, as check-update does.
            sandogasa_cli::require_tools(&[("koji", "sudo dnf install koji", None)])?;
            if opts.verbose {
                eprintln!("[rebuild-plan] listing {tag}");
            }
            Some(KojiCli { profile: None }.list_tagged(tag)?)
        }
        None => None,
    };
    let fedrq = sandogasa_fedrq::Fedrq {
        branch: Some(opts.branch.clone()),
        repo: None,
    };
    let side_tag = opts.side_tag.as_deref().zip(tagged.as_deref());
    let plan = plan(&opts.library, &opts.branch, side_tag, &fedrq, opts.verbose)?;
    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&plan).expect("JSON serialization failed")
        );
    } else {
        print!("{}", render(&plan));
    }
    Ok(())
}

impl PlanQuery for sandogasa_fedrq::Fedrq {
    fn subpkgs_provides(&self, srpm: &str) -> Result<Vec<String>, String> {
        sandogasa_fedrq::Fedrq::subpkgs_provides(self, srpm).map_err(|e| e.to_string())
    }

    fn subpkgs_names(&self, srpm: &str) -> Result<Vec<String>, String> {
        sandogasa_fedrq::Fedrq::subpkgs_names(self, srpm).map_err(|e| e.to_string())
    }

    fn whatrequires(&self, capabilities: &[String]) -> Result<Vec<String>, String> {
        sandogasa_fedrq::Fedrq::whatrequires(self, capabilities).map_err(|e| e.to_string())
    }

    fn whatrequires_src(&self, capabilities: &[String]) -> Result<Vec<String>, String> {
        sandogasa_fedrq::Fedrq::whatrequires_src(self, capabilities).map_err(|e| e.to_string())
    }

    fn src_requires(&self, srpm: &str) -> Result<Vec<String>, String> {
        sandogasa_fedrq::Fedrq::src_requires(self, srpm).map_err(|e| e.to_string())
    }

    fn providers(&self, deps: &[String]) -> Result<Vec<PkgInfo>, String> {
        sandogasa_fedrq::Fedrq::providers_info(self, deps).map_err(|e| e.to_string())
    }
}

/// Whether a provide is a shared library's SONAME, like
/// `libical.so.3()(64bit)` — not a symbol-version provide such as
/// `libfoo.so.1(FOO_1.0)(64bit)`, which every package needing it also
/// pairs with the plain SONAME.
fn is_soname(name: &str) -> bool {
    let base = name.strip_suffix("(64bit)").unwrap_or(name);
    base.ends_with("()") && base.contains(".so")
}

/// Split a library's provides into its SONAMEs and the capabilities a
/// package builds against: package names, `pkgconfig()`, `cmake()`.
/// Versions are dropped, so a requirement of any version matches.
fn split_provides(provides: &[String]) -> (Vec<String>, Vec<String>) {
    let mut sonames = BTreeSet::new();
    let mut build = BTreeSet::new();
    for pr in provides {
        let name = sandogasa_fedrq::dep_name(pr);
        if is_soname(name) {
            sonames.insert(name.to_string());
        } else if !name.contains(".so") {
            build.insert(name.to_string());
        }
    }
    (sonames.into_iter().collect(), build.into_iter().collect())
}

/// Whether a BuildRequire is one a repository can answer: `rpmlib()`
/// dependencies are satisfied by rpm itself, never by a package.
fn is_answerable(dep: &str) -> bool {
    !dep.starts_with("rpmlib(")
}

/// The packages among `packages` whose BuildRequires do not all
/// resolve, each with the packages it would strand.
///
/// Capability dependencies are asked in one batch and attributed with
/// [`PkgInfo::satisfies`], which also reads boolean dependencies. A
/// file dependency is asked on its own: a provider's Provides do not
/// list its files, so a batched answer cannot be attributed to it.
fn find_unbuildable(
    packages: &[&str],
    q: &dyn PlanQuery,
    verbose: bool,
) -> Result<Vec<Unbuildable>, String> {
    let mut brs: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for p in packages {
        let deps: Vec<String> = q
            .src_requires(p)?
            .into_iter()
            .filter(|d| is_answerable(d))
            .collect();
        brs.insert(p, deps);
    }
    let caps: Vec<String> = brs
        .values()
        .flatten()
        .filter(|d| !d.starts_with('/'))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if verbose {
        eprintln!(
            "[rebuild-plan] checking {} BuildRequires of {} package(s)",
            caps.len(),
            packages.len()
        );
    }
    let providers = q.providers(&caps)?;

    let mut out = Vec::new();
    for (p, deps) in brs {
        let mut unresolved = Vec::new();
        for d in deps {
            let resolves = if d.starts_with('/') {
                !q.providers(std::slice::from_ref(&d))?.is_empty()
            } else {
                providers.iter().any(|pi| pi.satisfies(&d))
            };
            if !resolves {
                unresolved.push(d);
            }
        }
        if unresolved.is_empty() {
            continue;
        }
        let stranded: Vec<String> = q
            .whatrequires(&q.subpkgs_names(p)?)?
            .into_iter()
            .filter(|s| s != p)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        out.push(Unbuildable {
            package: p.to_string(),
            unresolved,
            stranded,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Repository answers recorded from the libical 4 rebuild on
    /// rawhide (2026-11-03), trimmed to what the plan asks.
    #[derive(Default)]
    struct Recorded {
        provides: BTreeMap<String, Vec<String>>,
        subpkgs: BTreeMap<String, Vec<String>>,
        /// capability → source packages whose binaries require it.
        requiring: BTreeMap<String, Vec<String>>,
        /// capability → source packages whose BuildRequires name it.
        building: BTreeMap<String, Vec<String>>,
        brs: BTreeMap<String, Vec<String>>,
        /// Capabilities that resolve, as (provider, its provides).
        available: Vec<(String, Vec<String>)>,
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn union(map: &BTreeMap<String, Vec<String>>, keys: &[String]) -> Vec<String> {
        keys.iter()
            .flat_map(|k| map.get(k).cloned().unwrap_or_default())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    impl PlanQuery for Recorded {
        fn subpkgs_provides(&self, srpm: &str) -> Result<Vec<String>, String> {
            Ok(self.provides.get(srpm).cloned().unwrap_or_default())
        }
        fn subpkgs_names(&self, srpm: &str) -> Result<Vec<String>, String> {
            Ok(self.subpkgs.get(srpm).cloned().unwrap_or_default())
        }
        fn whatrequires(&self, capabilities: &[String]) -> Result<Vec<String>, String> {
            Ok(union(&self.requiring, capabilities))
        }
        fn whatrequires_src(&self, capabilities: &[String]) -> Result<Vec<String>, String> {
            Ok(union(&self.building, capabilities))
        }
        fn src_requires(&self, srpm: &str) -> Result<Vec<String>, String> {
            Ok(self.brs.get(srpm).cloned().unwrap_or_default())
        }
        fn providers(&self, deps: &[String]) -> Result<Vec<PkgInfo>, String> {
            Ok(self
                .available
                .iter()
                .map(|(name, provides)| {
                    PkgInfo::new(name.clone(), vec![], provides.clone(), None, "rawhide")
                })
                .filter(|pi| deps.iter().any(|d| pi.satisfies(d)))
                .collect())
        }
    }

    const RUNTIME: &[&str] = &[
        "almanah",
        "asterisk",
        "bijiben",
        "bluez",
        "cairo-dock-plug-ins",
        "claws-mail",
        "cyrus-imapd",
        "evolution",
        "evolution-data-server",
        "evolution-ews",
        "gnokii",
        "gnome-calendar",
        "gnome-panel",
        "gnome-shell",
        "gnome-todo",
        "GoldenCheetah",
        "kdepimlibs",
        "kf6-kcalendarcore",
        "kmymoney",
        "noctalia",
        "orage",
        "osmo",
        "phosh",
        "pidgin-chime",
        "redwax-tool",
        "syncevolution",
    ];

    const SIDE_TAG: &[&str] = &[
        "almanah-0.12.4-8.fc46",
        "bijiben-40.2-13.fc46",
        "bluez-5.87^20260918gitb5ed56d8c-4.fc46",
        "cairo-dock-plug-ins-3.6.2-10.fc46",
        "calls-51.1-2.fc46",
        "chatty-0.8.9-16.fc46",
        "evolution-3.62.1-3.fc46",
        "evolution-data-server-3.62.1-2.fc46",
        "evolution-ews-3.62.1-2.fc46",
        "evolution-rspam-0.6.0-68.fc46",
        "folks-0.15.12-12.fc46",
        "gdm-51.1.1-2.fc46",
        "geary-46.0-23.fc46",
        "glabels-3.4.1-36.fc46",
        "gnome-bluetooth-47.2-4.fc46",
        "gnome-calendar-51.0-4.fc46",
        "gnome-contacts-51.0-2.fc46",
        "gnome-initial-setup-51.1-3.fc46",
        "gnome-session-51.0-2.fc46",
        "gnome-shell-51.1-2.fc46",
        "gnome-shell-extension-background-logo-51~alpha-3.fc46",
        "gnome-shell-extensions-51.0-2.fc46",
        "gnome-todo-43.0-10.fc46",
        "libical-4.0.7-1.fc46",
        "pidgin-chime-1.5-17.fc46",
        "syncevolution-2.0.0-23.fc46",
    ];

    fn libical() -> Recorded {
        let mut r = Recorded::default();
        r.provides.insert(
            "libical".into(),
            strs(&[
                "libical = 3.0.20-11.fc46",
                "libical(aarch-64) = 3.0.20-11.fc46",
                "libical.so.3()(64bit)",
                "libical_cxx.so.3()(64bit)",
                "libicalss.so.3()(64bit)",
                "libicalss_cxx.so.3()(64bit)",
                "libicalvcal.so.3()(64bit)",
                "libical-glib.so.3()(64bit)",
                "libical-devel = 3.0.20-11.fc46",
                "libical-glib-devel = 3.0.20-11.fc46",
                "pkgconfig(libical) = 3.0.20",
                "pkgconfig(libical-glib) = 3.0.20",
                "cmake(libical) = 3.0.20",
                "cmake(LibIcal) = 3.0.20",
            ]),
        );
        let mut requiring = RUNTIME.to_vec();
        requiring.push("libical");
        r.requiring
            .insert("libical.so.3()(64bit)".into(), strs(&requiring));
        r.building.insert(
            "libical-devel".into(),
            strs(&[
                "asterisk",
                "cyrus-timezones",
                "kdepimlibs",
                "kf6-kcalendarcore",
            ]),
        );
        r.building.insert(
            "pkgconfig(libical-glib)".into(),
            strs(&["evolution-data-server", "gnome-calendar"]),
        );
        // kdepimlibs is KDE4: QJson left the distro long before libical 4.
        r.brs.insert(
            "kdepimlibs".into(),
            strs(&["cmake", "pkgconfig(QJson)", "libical-devel"]),
        );
        r.brs.insert(
            "kmymoney".into(),
            strs(&[
                "cmake",
                "rpmlib(CompressedFileNames) <= 3.0.4-1",
                "/usr/bin/xsltproc",
            ]),
        );
        r.brs.insert(
            "phosh".into(),
            strs(&["(pkgconfig(gtk4) >= 4.12 with pkgconfig(gtk4) < 5)"]),
        );
        r.available = vec![
            ("cmake".into(), strs(&["cmake = 3.31.6-1.fc46"])),
            (
                "libical-devel".into(),
                strs(&["libical-devel = 3.0.20-11.fc46"]),
            ),
            ("gtk4-devel".into(), strs(&["pkgconfig(gtk4) = 4.20.1"])),
            ("libxslt".into(), strs(&["libxslt = 1.1.43-1.fc46"])),
        ];
        r.subpkgs.insert(
            "kdepimlibs".into(),
            strs(&["kdepimlibs", "kdepimlibs-akonadi", "kdepimlibs-devel"]),
        );
        r.requiring
            .insert("kdepimlibs-akonadi".into(), strs(&["kuser", "kdepimlibs"]));
        r
    }

    /// File dependencies are answered one by one, and a provider's
    /// Provides never list its files, so the recorded repository
    /// answers `/usr/bin/xsltproc` by name.
    impl Recorded {
        fn with_file(mut self, path: &str, provider: &str) -> Self {
            self.available.push((provider.into(), strs(&[path])));
            self
        }
    }

    fn libical_plan() -> RebuildPlan {
        let tagged = strs(SIDE_TAG);
        let q = libical().with_file("/usr/bin/xsltproc", "libxslt");
        plan(
            "libical",
            "rawhide",
            Some(("f46-build-side-158330", &tagged)),
            &q,
            false,
        )
        .unwrap()
    }

    fn names(ds: &[&Dependent]) -> Vec<String> {
        ds.iter().map(|d| d.package.clone()).collect()
    }

    #[test]
    fn soname_provides_are_told_from_build_capabilities() {
        assert!(is_soname("libical.so.3()(64bit)"));
        assert!(is_soname("libical.so.3()"));
        assert!(!is_soname("libfoo.so.1(FOO_1.0)(64bit)"));
        assert!(!is_soname("pkgconfig(libical)"));
        let (sonames, build) = split_provides(&strs(&[
            "libical.so.3()(64bit)",
            "libfoo.so.1(FOO_1.0)(64bit)",
            "pkgconfig(libical) = 3.0.20",
            "libical-devel = 3.0.20-11.fc46",
        ]));
        assert_eq!(sonames, strs(&["libical.so.3()(64bit)"]));
        assert_eq!(build, strs(&["libical-devel", "pkgconfig(libical)"]));
    }

    #[test]
    fn libical_leaves_thirteen_to_rebuild() {
        let p = libical_plan();
        assert_eq!(
            names(&p.pending()),
            strs(&[
                "GoldenCheetah",
                "asterisk",
                "claws-mail",
                "cyrus-imapd",
                "gnokii",
                "gnome-panel",
                "kf6-kcalendarcore",
                "kmymoney",
                "noctalia",
                "orage",
                "osmo",
                "phosh",
                "redwax-tool",
            ])
        );
    }

    #[test]
    fn kdepimlibs_cannot_be_rebuilt_and_strands_kuser() {
        let p = libical_plan();
        assert_eq!(p.unbuildable.len(), 1, "{:?}", p.unbuildable);
        let u = &p.unbuildable[0];
        assert_eq!(u.package, "kdepimlibs");
        assert_eq!(u.unresolved, strs(&["pkgconfig(QJson)"]));
        assert_eq!(u.stranded, strs(&["kuser"]));
    }

    #[test]
    fn rpmlib_file_and_boolean_buildrequires_are_not_unresolved() {
        // kmymoney's rpmlib() and file dependency, and phosh's boolean
        // one, all resolve; neither may be reported as unbuildable.
        let p = libical_plan();
        assert!(!p.is_unbuildable("kmymoney"));
        assert!(!p.is_unbuildable("phosh"));
    }

    #[test]
    fn the_side_tag_says_what_is_done_and_what_was_not_needed() {
        let p = libical_plan();
        assert_eq!(
            names(&p.rebuilt()),
            strs(&[
                "almanah",
                "bijiben",
                "bluez",
                "cairo-dock-plug-ins",
                "evolution",
                "evolution-data-server",
                "evolution-ews",
                "gnome-calendar",
                "gnome-shell",
                "gnome-todo",
                "pidgin-chime",
                "syncevolution",
            ])
        );
        assert_eq!(p.rebuilt_not_needed.len(), 13, "{:?}", p.rebuilt_not_needed);
        assert!(p.rebuilt_not_needed.contains(&"folks".to_string()));
        assert!(!p.rebuilt_not_needed.contains(&"libical".to_string()));
    }

    #[test]
    fn a_build_only_dependent_is_listed_apart() {
        let p = libical_plan();
        assert_eq!(names(&p.build_only()), strs(&["cyrus-timezones"]));
        assert!(!names(&p.pending()).contains(&"cyrus-timezones".to_string()));
    }

    #[test]
    fn without_a_side_tag_every_dependent_needs_a_rebuild() {
        let q = libical().with_file("/usr/bin/xsltproc", "libxslt");
        let p = plan("libical", "rawhide", None, &q, false).unwrap();
        // 26 runtime dependents, less kdepimlibs, which cannot build.
        assert_eq!(p.pending().len(), 25);
        assert!(p.rebuilt_not_needed.is_empty());
        assert!(render(&p).contains("## Needs a rebuild (25)"));
    }

    #[test]
    fn a_library_without_a_soname_is_refused() {
        let mut q = Recorded::default();
        q.provides
            .insert("python-foo".into(), strs(&["python3-foo = 1.0-1.fc46"]));
        let err = plan("python-foo", "rawhide", None, &q, false).unwrap_err();
        assert!(err.contains("no SONAME"), "{err}");
        let err = plan("missing", "rawhide", None, &q, false).unwrap_err();
        assert!(err.contains("no binary packages"), "{err}");
    }

    #[test]
    fn the_markdown_names_each_section() {
        let out = render(&libical_plan());
        for want in [
            "# Rebuild plan for libical (rawhide)",
            "Side tag: `f46-build-side-158330`",
            "## Still to rebuild (13)",
            "## Cannot be rebuilt (1)",
            "- kdepimlibs: `pkgconfig(QJson)`. Once the new libical lands, these become \
             uninstallable with it: kuser",
            "## Already rebuilt in f46-build-side-158330 (12)",
            "- gnome-shell (`gnome-shell-51.1-2.fc46`)",
            "## Build-time only (1)",
            "## Rebuilt but not using libical (13)",
        ] {
            assert!(out.contains(want), "missing {want:?} in:\n{out}");
        }
    }

    #[test]
    fn the_json_carries_links_and_builds() {
        let v = serde_json::to_value(libical_plan()).unwrap();
        let shell = v["dependents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["package"] == "gnome-shell")
            .unwrap();
        assert_eq!(shell["link"], "runtime");
        assert_eq!(shell["rebuilt"], "gnome-shell-51.1-2.fc46");
        assert_eq!(v["unbuildable"][0]["stranded"][0], "kuser");
    }
}
