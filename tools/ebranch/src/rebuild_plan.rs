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

/// How a dependent's build gets the library's development files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reach {
    /// Its BuildRequires name them.
    Declared,
    /// Through another package's development files. `path` starts at
    /// the BuildRequire and names each binary package on the way to
    /// the library's (`pkgconfig(libecal-2.0)`,
    /// `evolution-data-server-devel`, `libical-glib-devel`).
    Through { path: Vec<String> },
    /// None of its BuildRequires leads to them as far as the plan
    /// follows dependencies (it does not follow file dependencies).
    Unexplained,
    /// It does not link the library, but links a SONAME that
    /// `package`'s rebuild in the side tag dropped.
    Relinks { package: String },
}

/// One package that uses the library.
#[derive(Debug, Clone, Serialize)]
pub struct Dependent {
    pub package: String,
    pub link: Link,
    pub reach: Reach,
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

/// The side tag the rebuilds go into, as a plan reads it.
pub(crate) struct SideTag {
    pub name: String,
    /// The builds tagged into it.
    pub nvrs: Vec<String>,
    /// Source package → the SONAMEs its build in the side tag provides,
    /// for each build whose binaries could be read.
    pub sonames: BTreeMap<String, BTreeSet<String>>,
}

/// A rebuild in the side tag that dropped SONAMEs of its own, so its
/// runtime dependents need rebuilding as well.
#[derive(Debug, Clone, Serialize)]
pub struct SonameChange {
    pub package: String,
    /// SONAMEs the branch has and the rebuild no longer provides.
    pub removed: Vec<String>,
    /// Packages requiring them, which join the plan's dependents.
    pub dependents: Vec<String>,
}

/// Packages that may compile the library's headers in without linking
/// it, through development packages that require the library's.
#[derive(Debug, Clone, Serialize)]
pub struct HeaderExposure {
    /// A development package requiring one of the library's directly.
    pub through: String,
    /// Development packages requiring `through`, directly or further
    /// down.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub further: Vec<String>,
    /// Source package → the development packages among `through` and
    /// `further` that it BuildRequires.
    pub packages: BTreeMap<String, Vec<String>>,
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
    /// The packages still to rebuild, in rounds: a round needs only
    /// packages from earlier rounds in its buildroot.
    pub rounds: Vec<Vec<String>>,
    /// Package → the packages still to rebuild that its buildroot
    /// needs first.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub waits_for: BTreeMap<String, Vec<String>>,
    /// Package → the packages that cannot be rebuilt but which its
    /// buildroot needs: it cannot be built until they are fixed.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub blocked_by: BTreeMap<String, Vec<String>>,
    /// Packages still to rebuild that need each other's rebuilds in
    /// their buildroots. Each cycle is one round in `rounds`, and has to
    /// be broken by hand (a bootstrap build, or a temporary
    /// BuildRequires change).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cycles: Vec<Vec<String>>,
    /// Rebuilds in the side tag that dropped SONAMEs of their own.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub soname_changes: Vec<SonameChange>,
    /// Rebuilds in the side tag whose SONAMEs could not be compared:
    /// its repository had none of their binaries for this architecture.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub not_compared: Vec<String>,
    /// Packages that neither use the library nor are rebuilt, but
    /// BuildRequire development packages that bring its headers, by
    /// the development package of the library's first reached. Worth a
    /// scratch build.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub header_exposed: Vec<HeaderExposure>,
}

impl RebuildPlan {
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

    /// Dependents linking the library whose BuildRequires do not name
    /// it.
    pub fn undeclared(&self) -> Vec<&Dependent> {
        self.dependents
            .iter()
            .filter(|d| matches!(d.reach, Reach::Through { .. } | Reach::Unexplained))
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

    /// Binary packages requiring any of the packages, as
    /// `(binary, source)`; source packages are left out.
    fn bin_requirers(&self, packages: &[String]) -> Result<Vec<(String, String)>, String>;

    /// The binary packages of the source packages, with their
    /// Provides, in one batch.
    fn subpkgs_provides_info(&self, srpms: &[String]) -> Result<Vec<PkgInfo>, String>;
}

/// Work out the plan for `library` on the branch `q` reads, with the
/// side tag's builds when there is one.
pub(crate) fn plan(
    library: &str,
    branch: &str,
    side_tag: Option<&SideTag>,
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

    let tagged: BTreeMap<String, String> = side_tag
        .map(|st| {
            st.nvrs
                .iter()
                .filter_map(|nvr| {
                    sandogasa_koji::parse_nvr_name(nvr).map(|n| (n.to_string(), nvr.clone()))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut runtime: BTreeSet<String> = q
        .whatrequires(&sonames)?
        .into_iter()
        .filter(|p| p != library)
        .collect();
    let (soname_changes, not_compared) = match side_tag {
        Some(st) => soname_changes(library, st, &tagged, q, verbose)?,
        None => (Vec::new(), Vec::new()),
    };
    // Packages that only follow a rebuild's SONAME change, with it.
    let mut relinks: BTreeMap<String, String> = BTreeMap::new();
    for c in &soname_changes {
        for d in &c.dependents {
            if !runtime.contains(d) {
                relinks
                    .entry(d.clone())
                    .or_insert_with(|| c.package.clone());
            }
        }
    }
    runtime.extend(relinks.keys().cloned());
    let stale: Vec<String> = sonames
        .iter()
        .chain(soname_changes.iter().flat_map(|c| &c.removed))
        .cloned()
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

    let undeclared: Vec<&str> = runtime
        .difference(&build)
        .filter(|p| !relinks.contains_key(*p))
        .map(String::as_str)
        .collect();
    let to_check: Vec<&str> = runtime
        .iter()
        .filter(|p| !tagged.contains_key(*p))
        .map(String::as_str)
        .collect();
    let all_brs = fetch_brs(
        &undeclared
            .iter()
            .chain(&to_check)
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>(),
        q,
    )?;

    let mut closure = Closure::default();
    let dev = closure.development_packages(library, &build_caps, q)?;
    let mut dependents = Vec::new();
    for p in runtime.union(&build) {
        let reach = if build.contains(p) {
            Reach::Declared
        } else if let Some(package) = relinks.get(p) {
            Reach::Relinks {
                package: package.clone(),
            }
        } else {
            let brs = all_brs.get(p).map(Vec::as_slice).unwrap_or_default();
            match closure.route(brs, &dev, q)? {
                Some(path) => Reach::Through { path },
                None => Reach::Unexplained,
            }
        };
        if verbose && reach != Reach::Declared {
            eprintln!("[rebuild-plan] {p} does not BuildRequire {library}: {reach:?}");
        }
        dependents.push(Dependent {
            package: p.clone(),
            link: if runtime.contains(p) {
                Link::Runtime
            } else {
                Link::BuildOnly
            },
            reach,
            rebuilt: tagged.get(p).cloned(),
        });
    }

    let rebuilt_not_needed: Vec<String> = tagged
        .keys()
        .filter(|p| *p != library && !runtime.contains(*p) && !build.contains(*p))
        .cloned()
        .collect();

    let brs: BTreeMap<String, Vec<String>> = all_brs
        .into_iter()
        .filter(|(p, _)| to_check.contains(&p.as_str()))
        .collect();
    let unbuildable = find_unbuildable(&brs, q, verbose)?;
    let cannot: BTreeSet<String> = unbuildable.iter().map(|u| u.package.clone()).collect();
    let order = build_order(&brs, &cannot, &stale, &mut closure, q, verbose)?;

    let mut settled: BTreeSet<String> = runtime.union(&build).cloned().collect();
    settled.extend(tagged.keys().cloned());
    settled.insert(library.to_string());
    let header_exposed = header_exposure(library, &dev, &settled, q, verbose)?;

    Ok(RebuildPlan {
        library: library.to_string(),
        branch: branch.to_string(),
        side_tag: side_tag.map(|st| st.name.clone()),
        sonames,
        dependents,
        unbuildable,
        rebuilt_not_needed,
        rounds: order.rounds,
        waits_for: order.waits_for,
        blocked_by: order.blocked_by,
        cycles: order.cycles,
        soname_changes,
        not_compared,
        header_exposed,
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

    let heading = if plan.side_tag.is_some() {
        "Still to rebuild"
    } else {
        "Needs a rebuild"
    };
    let ready: usize = plan.rounds.iter().map(Vec::len).sum();
    let _ = writeln!(out, "## {heading} ({ready})\n");
    render_rounds(&mut out, plan);

    if !plan.blocked_by.is_empty() {
        let _ = writeln!(out, "## Blocked ({})\n", plan.blocked_by.len());
        let _ = writeln!(
            out,
            "Their buildroots need a package that cannot be rebuilt (below), \
             so they cannot be built until it is fixed:\n"
        );
        for (p, by) in &plan.blocked_by {
            let _ = writeln!(out, "- {p}: needs {}", by.join(", "));
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

    if !plan.soname_changes.is_empty() {
        let _ = writeln!(
            out,
            "## Rebuilds that changed a SONAME of their own ({})\n",
            plan.soname_changes.len()
        );
        let _ = writeln!(
            out,
            "Their rebuilds in the side tag dropped these SONAMEs, so what links them \
             needs rebuilding too; those packages are in the plan above:\n"
        );
        for c in &plan.soname_changes {
            let removed: Vec<String> = c.removed.iter().map(|s| format!("`{s}`")).collect();
            let _ = write!(out, "- {}: drops {}", c.package, removed.join(", "));
            if !c.dependents.is_empty() {
                let _ = write!(out, "; linked by {}", c.dependents.join(", "));
            }
            out.push('\n');
        }
        out.push('\n');
    }

    let undeclared = plan.undeclared();
    if !undeclared.is_empty() {
        let _ = writeln!(
            out,
            "## Linking {} without a BuildRequires on it ({})\n",
            plan.library,
            undeclared.len()
        );
        let _ = writeln!(
            out,
            "They get its development files through another package's, so a search of \
             BuildRequires misses them:\n"
        );
        for d in &undeclared {
            match &d.reach {
                Reach::Through { path } => {
                    let (br, rest) = path
                        .split_first()
                        .expect("a route starts at a BuildRequire");
                    let _ = write!(out, "- {}: `{br}`", d.package);
                    for name in rest {
                        let _ = write!(out, " → {name}");
                    }
                }
                _ => {
                    let _ = write!(
                        out,
                        "- {}: none of its BuildRequires leads to {} as far as the plan \
                         follows them (file dependencies are not followed)",
                        d.package, plan.library
                    );
                }
            }
            if d.rebuilt.is_some() {
                out.push_str(" (rebuilt)");
            }
            out.push('\n');
        }
        out.push('\n');
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

    if !plan.header_exposed.is_empty() {
        let n: BTreeSet<&String> = plan
            .header_exposed
            .iter()
            .flat_map(|e| e.packages.keys())
            .collect();
        let _ = writeln!(
            out,
            "## May compile in {}'s headers ({})\n",
            plan.library,
            n.len()
        );
        let _ = writeln!(
            out,
            "These neither link {lib} nor are rebuilt, but BuildRequire development files that \
             require {lib}'s, so its headers may reach them (inline functions, macros, struct \
             layouts) without a dependency on its SONAME. Worth a scratch build. A development \
             package requiring {lib}'s is only a sign: if its own headers do not include {lib}'s, \
             the whole group below it can be ruled out at once.\n",
            lib = plan.library
        );
        for e in &plan.header_exposed {
            let _ = writeln!(out, "### Through {} ({})\n", e.through, e.packages.len());
            if !e.further.is_empty() {
                let _ = writeln!(
                    out,
                    "And the development packages requiring it: {}\n",
                    e.further.join(", ")
                );
            }
            for (p, via) in &e.packages {
                if via.len() == 1 && via[0] == e.through {
                    let _ = writeln!(out, "- {p}");
                } else {
                    let _ = writeln!(out, "- {p} ({})", via.join(", "));
                }
            }
            out.push('\n');
        }
    }

    if plan.side_tag.is_some() {
        if !plan.not_compared.is_empty() {
            let _ = writeln!(
                out,
                "Not checked for SONAME changes, as the side tag's repository has none of \
                 their binaries for this architecture (an ExcludeArch, or a repository not \
                 regenerated since the build): {}\n",
                plan.not_compared.join(", ")
            );
        }
        if plan.soname_changes.is_empty() {
            let checked = if plan.not_compared.is_empty() {
                "No rebuild"
            } else {
                "No other rebuild"
            };
            let _ = writeln!(
                out,
                "{checked} in the side tag changed a SONAME of its own, so nothing further \
                 down a dependency chain needs rebuilding. Rerun this as rebuilds land."
            );
        }
    } else {
        let _ = writeln!(
            out,
            "Packages further down a dependency chain need a rebuild only if a rebuilt \
             package changes a SONAME of its own. With `--side-tag`, the plan checks the \
             rebuilds there and adds what follows."
        );
    }
    out
}

/// The packages still to rebuild, by round, saying what each waits for.
fn render_rounds(out: &mut String, plan: &RebuildPlan) {
    match plan.rounds.as_slice() {
        [] => {
            let _ = writeln!(out, "Nothing.\n");
        }
        [only] => {
            let _ = writeln!(
                out,
                "None of these needs another in its buildroot, so they can all be built at once:\n"
            );
            for p in only {
                let _ = writeln!(out, "- {p}");
            }
            out.push('\n');
        }
        rounds => {
            for (i, round) in rounds.iter().enumerate() {
                let _ = writeln!(out, "### Round {} ({})\n", i + 1, round.len());
                for p in round {
                    match plan.waits_for.get(p) {
                        Some(w) => {
                            let _ = writeln!(out, "- {p} (after {})", w.join(", "));
                        }
                        None => {
                            let _ = writeln!(out, "- {p}");
                        }
                    }
                }
                out.push('\n');
            }
        }
    }
    for c in &plan.cycles {
        let _ = writeln!(
            out,
            "Cycle: {} need each other's rebuilds in their buildroots; break it by hand \
             (a bootstrap build, or a temporary BuildRequires change).\n",
            c.join(", ")
        );
    }
}

/// Plan the rebuilds and print the result.
pub fn run(opts: &Options) -> Result<(), String> {
    sandogasa_cli::require_tools(&[("fedrq", "sudo dnf install fedrq", Some("--version"))])?;
    let side_tag = match &opts.side_tag {
        Some(tag) => {
            // `koji` has no `--version`, and `koji version` asks the hub, so
            // only its presence is checked, as check-update does.
            sandogasa_cli::require_tools(&[("koji", "sudo dnf install koji", None)])?;
            Some(read_side_tag(
                tag,
                &KojiCli { profile: None },
                opts.verbose,
            )?)
        }
        None => None,
    };
    let fedrq = sandogasa_fedrq::Fedrq {
        branch: Some(opts.branch.clone()),
        repo: None,
    };
    let plan = plan(
        &opts.library,
        &opts.branch,
        side_tag.as_ref(),
        &fedrq,
        opts.verbose,
    )?;
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

    fn bin_requirers(&self, packages: &[String]) -> Result<Vec<(String, String)>, String> {
        sandogasa_fedrq::Fedrq::whatrequires_notsrc(self, packages).map_err(|e| e.to_string())
    }

    fn subpkgs_provides_info(&self, srpms: &[String]) -> Result<Vec<PkgInfo>, String> {
        sandogasa_fedrq::Fedrq::subpkgs_provides_info(self, srpms).map_err(|e| e.to_string())
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

/// Source package → the SONAMEs its build among `nvrs` provides, read
/// from `infos`. A binary counts only when its version and release are
/// the tagged build's: a side tag's repository also carries what it
/// inherits.
fn sonames_by_source(nvrs: &[String], infos: &[PkgInfo]) -> BTreeMap<String, BTreeSet<String>> {
    let builds: BTreeMap<&str, (&str, &str)> = nvrs
        .iter()
        .filter_map(|nvr| sandogasa_koji::parse_nvr(nvr).map(|(n, v, r)| (n, (v, r))))
        .collect();
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for pi in infos {
        let Some(src) = pi.source_name.as_deref() else {
            continue;
        };
        let Some((v, r)) = builds.get(src) else {
            continue;
        };
        if pi.version.as_deref() != Some(*v) || pi.release.as_deref() != Some(*r) {
            continue;
        }
        let entry = out.entry(src.to_string()).or_default();
        entry.extend(
            pi.provides
                .iter()
                .map(|p| sandogasa_fedrq::dep_name(p))
                .filter(|n| is_soname(n))
                .map(str::to_string),
        );
    }
    out
}

/// The side tag's builds, and the SONAMEs each provides there: the
/// binary package names from Koji, then their Provides from the side
/// tag's repository in one query.
fn read_side_tag(tag: &str, koji: &dyn KojiQuery, verbose: bool) -> Result<SideTag, String> {
    if verbose {
        eprintln!("[rebuild-plan] listing {tag}");
    }
    let nvrs = koji.list_tagged(tag)?;
    let rpms: BTreeMap<String, Vec<String>> = nvrs
        .iter()
        .map(|nvr| (nvr.clone(), koji.build_rpms(nvr)))
        .collect();
    let names: Vec<String> = rpms
        .values()
        .flatten()
        .filter(|n| !n.ends_with("-debuginfo") && !n.ends_with("-debugsource"))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if verbose {
        eprintln!(
            "[rebuild-plan] reading the Provides of {} binary package(s) in {tag}",
            names.len()
        );
    }
    // The side tag's repository changes as builds land, and its
    // metadata is small: refetch it every run, as check-update does,
    // or builds tagged since the last run look absent.
    match sandogasa_fedrq::expire_repo_cache(&sandogasa_fedrq::koji_repoid_prefix(tag)) {
        Ok(n) if verbose => {
            eprintln!("[rebuild-plan] expired {n} cached metadata file(s) of {tag}")
        }
        Err(e) => eprintln!("warning: could not expire the cached metadata of {tag}: {e}"),
        _ => {}
    }
    let side = sandogasa_fedrq::Fedrq {
        branch: None,
        repo: Some(format!("@koji:{tag}")),
    };
    let infos = side.providers_vr_info(&names).map_err(|e| e.to_string())?;
    let sonames = sonames_by_source(&nvrs, &infos);
    Ok(SideTag {
        name: tag.to_string(),
        nvrs,
        sonames,
    })
}

/// Whether a BuildRequire is one a repository can answer: `rpmlib()`
/// dependencies are satisfied by rpm itself, never by a package.
fn is_answerable(dep: &str) -> bool {
    !dep.starts_with("rpmlib(")
}

/// Each package's BuildRequires, less those rpm answers itself.
fn fetch_brs(
    packages: &[&str],
    q: &dyn PlanQuery,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut brs = BTreeMap::new();
    for p in packages {
        let deps: Vec<String> = q
            .src_requires(p)?
            .into_iter()
            .filter(|d| is_answerable(d))
            .collect();
        brs.insert(p.to_string(), deps);
    }
    Ok(brs)
}

/// The packages among those in `brs` whose BuildRequires do not all
/// resolve, each with the packages it would strand.
///
/// Capability dependencies are asked in one batch and attributed with
/// [`PkgInfo::satisfies`], which also reads boolean dependencies. A
/// file dependency is asked on its own: a provider's Provides do not
/// list its files, so a batched answer cannot be attributed to it.
fn find_unbuildable(
    brs: &BTreeMap<String, Vec<String>>,
    q: &dyn PlanQuery,
    verbose: bool,
) -> Result<Vec<Unbuildable>, String> {
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
            brs.len()
        );
    }
    let providers = q.providers(&caps)?;

    let mut out = Vec::new();
    for (p, deps) in brs {
        let mut unresolved = Vec::new();
        for d in deps.iter().cloned() {
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

/// Give up following Requires after this many waves; a real
/// buildroot settles within a dozen.
const MAX_WAVES: usize = 64;

/// Dependency answers shared by every package's buildroot walk, so a
/// capability is asked once however many buildroots need it.
#[derive(Default)]
struct Closure {
    /// dependency → names of the binary packages providing it.
    providers: BTreeMap<String, Vec<String>>,
    /// binary package name → what it requires and its source.
    pkgs: BTreeMap<String, PkgInfo>,
}

impl Closure {
    /// Ask about the dependencies not answered yet, in one batch.
    fn resolve(&mut self, deps: &[String], q: &dyn PlanQuery) -> Result<(), String> {
        let unknown: Vec<String> = deps
            .iter()
            .filter(|d| !self.providers.contains_key(*d))
            .cloned()
            .collect();
        if unknown.is_empty() {
            return Ok(());
        }
        let infos = q.providers(&unknown)?;
        for d in unknown {
            let names = infos
                .iter()
                .filter(|pi| pi.satisfies(&d))
                .map(|pi| pi.name.clone())
                .collect();
            self.providers.insert(d, names);
        }
        for pi in infos {
            self.pkgs.entry(pi.name.clone()).or_insert(pi);
        }
        Ok(())
    }

    /// The library's binary packages that carry its development files:
    /// those providing one of its build capabilities whose name ends in
    /// `-devel` or which provide a `pkgconfig()` or `cmake()` module.
    fn development_packages(
        &mut self,
        library: &str,
        build_caps: &[String],
        q: &dyn PlanQuery,
    ) -> Result<BTreeSet<String>, String> {
        self.resolve(build_caps, q)?;
        Ok(self
            .pkgs
            .values()
            .filter(|pi| pi.source_name.as_deref() == Some(library))
            .filter(|pi| {
                pi.name.ends_with("-devel")
                    || pi.provides.iter().any(|p| {
                        let n = sandogasa_fedrq::dep_name(p);
                        n.starts_with("pkgconfig(") || n.starts_with("cmake(")
                    })
            })
            .map(|pi| pi.name.clone())
            .collect())
    }

    /// The shortest way from one of `brs` to one of the `targets`
    /// binary packages: the BuildRequire, then each binary package on
    /// the way. `None` when no BuildRequire leads there.
    fn route(
        &mut self,
        brs: &[String],
        targets: &BTreeSet<String>,
        q: &dyn PlanQuery,
    ) -> Result<Option<Vec<String>>, String> {
        let followed = |d: &String| is_answerable(d) && !d.starts_with('/');
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut frontier: Vec<(String, Vec<String>)> = brs
            .iter()
            .filter(|d| followed(d))
            .map(|d| (d.clone(), vec![d.clone()]))
            .collect();
        for _ in 0..MAX_WAVES {
            if frontier.is_empty() {
                break;
            }
            let deps: Vec<String> = frontier.iter().map(|(d, _)| d.clone()).collect();
            self.resolve(&deps, q)?;
            let mut next = Vec::new();
            for (d, path) in &frontier {
                for name in self.providers.get(d).into_iter().flatten() {
                    if !seen.insert(name.clone()) {
                        continue;
                    }
                    let mut to = path.clone();
                    to.push(name.clone());
                    if targets.contains(name) {
                        return Ok(Some(to));
                    }
                    if let Some(pi) = self.pkgs.get(name) {
                        next.extend(
                            pi.requires
                                .iter()
                                .filter(|r| followed(r))
                                .map(|r| (r.clone(), to.clone())),
                        );
                    }
                }
            }
            frontier = next;
        }
        Ok(None)
    }

    /// The binary packages a buildroot for `brs` installs: the
    /// BuildRequires' providers and, transitively, what those require.
    ///
    /// File dependencies are not followed, since a batched answer
    /// cannot be attributed to a path; one that leads to the library
    /// only through a file path is missed.
    fn buildroot(&mut self, brs: &[String], q: &dyn PlanQuery) -> Result<BTreeSet<String>, String> {
        let mut seen_deps: BTreeSet<String> = BTreeSet::new();
        let mut installed: BTreeSet<String> = BTreeSet::new();
        let mut frontier: Vec<String> = brs.to_vec();
        for _ in 0..MAX_WAVES {
            frontier.retain(|d| is_answerable(d) && !d.starts_with('/'));
            frontier.retain(|d| seen_deps.insert(d.clone()));
            if frontier.is_empty() {
                break;
            }
            self.resolve(&frontier, q)?;
            let mut next = Vec::new();
            for d in &frontier {
                for name in self.providers.get(d).into_iter().flatten() {
                    if installed.insert(name.clone())
                        && let Some(pi) = self.pkgs.get(name)
                    {
                        next.extend(pi.requires.iter().cloned());
                    }
                }
            }
            frontier = next;
        }
        Ok(installed)
    }
}

/// How the packages still to rebuild order among themselves.
struct Order {
    rounds: Vec<Vec<String>>,
    waits_for: BTreeMap<String, Vec<String>>,
    blocked_by: BTreeMap<String, Vec<String>>,
    cycles: Vec<Vec<String>>,
}

/// Order the packages in `brs` by what their buildroots need.
///
/// Until a package is rebuilt, its binaries that require one of the
/// old SONAMEs cannot be installed beside the new library. So a
/// package whose buildroot installs such a binary has to wait for that
/// binary's source to be rebuilt first, or, when the source cannot be
/// rebuilt, is blocked behind it. Binaries without a SONAME
/// requirement of their own are not a reason to wait.
fn build_order(
    brs: &BTreeMap<String, Vec<String>>,
    cannot: &BTreeSet<String>,
    sonames: &[String],
    closure: &mut Closure,
    q: &dyn PlanQuery,
    verbose: bool,
) -> Result<Order, String> {
    let stale: BTreeSet<&str> = brs.keys().map(String::as_str).collect();
    let mut needs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (p, deps) in brs {
        if cannot.contains(p) {
            continue;
        }
        let installed = closure.buildroot(deps, q)?;
        let sources: BTreeSet<String> = installed
            .iter()
            .filter_map(|name| closure.pkgs.get(name))
            .filter(|pi| {
                pi.requires
                    .iter()
                    .any(|r| sonames.iter().any(|s| s == sandogasa_fedrq::dep_name(r)))
            })
            .filter_map(|pi| pi.source_name.clone())
            .filter(|src| src != p && stale.contains(src.as_str()))
            .collect();
        if verbose {
            eprintln!(
                "[rebuild-plan] {p}: buildroot of {} package(s), waits for {}",
                installed.len(),
                if sources.is_empty() {
                    "nothing".to_string()
                } else {
                    sources.iter().cloned().collect::<Vec<_>>().join(", ")
                }
            );
        }
        needs.insert(p.clone(), sources);
    }

    // A package needing one that cannot be rebuilt is blocked, and so
    // is anything waiting for a blocked package.
    let mut blocked_by: BTreeMap<String, Vec<String>> = BTreeMap::new();
    loop {
        let mut grew = false;
        for (p, srcs) in &needs {
            if blocked_by.contains_key(p) {
                continue;
            }
            let by: Vec<String> = srcs
                .iter()
                .filter(|s| cannot.contains(*s) || blocked_by.contains_key(*s))
                .cloned()
                .collect();
            if !by.is_empty() {
                blocked_by.insert(p.clone(), by);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }

    let edges: BTreeMap<String, BTreeSet<String>> = needs
        .iter()
        .filter(|(p, _)| !blocked_by.contains_key(*p))
        .map(|(p, srcs)| {
            let waits = srcs
                .iter()
                .filter(|s| !cannot.contains(*s) && !blocked_by.contains_key(*s))
                .cloned()
                .collect();
            (p.clone(), waits)
        })
        .collect();
    let cycles: Vec<Vec<String>> = crate::dag::find_cycles(&edges)
        .into_iter()
        .map(|c| c.packages)
        .collect();
    // Build each cycle as one round: drop the edges inside it so the
    // rest can still be layered.
    let mut layered = edges.clone();
    for c in &cycles {
        for p in c {
            if let Some(w) = layered.get_mut(p) {
                w.retain(|x| !c.contains(x));
            }
        }
    }
    let rounds = crate::dag::topological_layers(&layered)
        .map_err(|left| format!("build order did not settle for: {}", left.join(", ")))?
        .into_iter()
        .map(|phase| phase.packages)
        .collect();
    let waits_for = edges
        .into_iter()
        .filter(|(_, w)| !w.is_empty())
        .map(|(p, w)| (p, w.into_iter().collect()))
        .collect();
    Ok(Order {
        rounds,
        waits_for,
        blocked_by,
        cycles,
    })
}

/// The rebuilds in the side tag that dropped SONAMEs the branch has,
/// and the packages linking them. The library's own build is left out:
/// its SONAMEs are the bump being planned.
///
/// Only builds whose binaries the side tag answered for are compared,
/// so a lookup that failed reads as "unknown", not as every SONAME
/// dropped.
fn soname_changes(
    library: &str,
    st: &SideTag,
    tagged: &BTreeMap<String, String>,
    q: &dyn PlanQuery,
    verbose: bool,
) -> Result<(Vec<SonameChange>, Vec<String>), String> {
    let (compared, not_compared): (Vec<String>, Vec<String>) = tagged
        .keys()
        .filter(|p| *p != library)
        .cloned()
        .partition(|p| st.sonames.contains_key(p));
    if verbose && !not_compared.is_empty() {
        eprintln!(
            "[rebuild-plan] not in the side tag's repository, so not compared: {}",
            not_compared.join(", ")
        );
    }
    let mut on_branch: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for pi in q.subpkgs_provides_info(&compared)? {
        if let Some(src) = pi.source_name {
            on_branch.entry(src).or_default().extend(pi.provides);
        }
    }
    let mut out = Vec::new();
    for package in &compared {
        let new = &st.sonames[package];
        let (old, _) = split_provides(
            on_branch
                .get(package)
                .map(Vec::as_slice)
                .unwrap_or_default(),
        );
        let removed: Vec<String> = old.into_iter().filter(|s| !new.contains(s)).collect();
        if removed.is_empty() {
            continue;
        }
        let dependents: Vec<String> = q
            .whatrequires(&removed)?
            .into_iter()
            .filter(|d| d != package && d != library)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if verbose {
            eprintln!(
                "[rebuild-plan] {package} dropped {}; linked by {}",
                removed.join(", "),
                dependents.len()
            );
        }
        out.push(SonameChange {
            package: package.clone(),
            removed,
            dependents,
        });
    }
    Ok((out, not_compared))
}

/// Packages that may compile the library's headers in: walk from the
/// library's development packages (`dev`) to the development packages
/// requiring them, then on to those requiring those, and take the
/// source packages BuildRequiring any of them that are not `settled`
/// (using the library, rebuilt, or the library itself).
///
/// Only `-devel` packages are followed. Following every requirer would
/// reach most of the distribution through runtime libraries, while
/// headers travel through development packages.
fn header_exposure(
    library: &str,
    dev: &BTreeSet<String>,
    settled: &BTreeSet<String>,
    q: &dyn PlanQuery,
    verbose: bool,
) -> Result<Vec<HeaderExposure>, String> {
    let devel_requirers = |names: &[String]| -> Result<BTreeSet<String>, String> {
        Ok(q.bin_requirers(names)?
            .into_iter()
            .filter(|(bin, src)| src != library && bin.ends_with("-devel"))
            .map(|(bin, _)| bin)
            .collect())
    };
    let first = devel_requirers(&dev.iter().cloned().collect::<Vec<_>>())?;
    let mut out = Vec::new();
    for through in first {
        let mut further = BTreeSet::new();
        let mut frontier = vec![through.clone()];
        for _ in 0..MAX_WAVES {
            let next: Vec<String> = devel_requirers(&frontier)?
                .into_iter()
                .filter(|b| *b != through && further.insert(b.clone()))
                .collect();
            if next.is_empty() {
                break;
            }
            frontier = next;
        }

        let mut packages: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for d in std::iter::once(&through).chain(&further) {
            for src in q.whatrequires_src(std::slice::from_ref(d))? {
                if !settled.contains(&src) {
                    packages.entry(src).or_default().push(d.clone());
                }
            }
        }
        if verbose {
            eprintln!(
                "[rebuild-plan] through {through}: {} more development package(s), {} package(s) to scratch-build",
                further.len(),
                packages.len()
            );
        }
        if !packages.is_empty() {
            out.push(HeaderExposure {
                through,
                further: further.into_iter().collect(),
                packages,
            });
        }
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
        /// Binary packages on the branch, as the providers query
        /// returns them.
        available: Vec<PkgInfo>,
        /// package → binary packages requiring it, as `(binary, source)`.
        bin_requiring: BTreeMap<String, Vec<(String, String)>>,
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// A binary package: its name, source, Provides and Requires. Its
    /// own name is provided, as every RPM provides its name.
    fn bin(name: &str, source: &str, provides: &[&str], requires: &[&str]) -> PkgInfo {
        let mut pr = strs(provides);
        pr.push(name.to_string());
        PkgInfo::new(
            name,
            strs(requires),
            pr,
            Some(source.to_string()),
            "rawhide",
        )
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
                .filter(|pi| deps.iter().any(|d| pi.satisfies(d)))
                .cloned()
                .collect())
        }
        fn bin_requirers(&self, packages: &[String]) -> Result<Vec<(String, String)>, String> {
            Ok(packages
                .iter()
                .flat_map(|p| self.bin_requiring.get(p).cloned().unwrap_or_default())
                .collect())
        }
        /// One binary package per source, carrying all its Provides.
        fn subpkgs_provides_info(&self, srpms: &[String]) -> Result<Vec<PkgInfo>, String> {
            Ok(srpms
                .iter()
                .filter_map(|s| {
                    let provides = self.provides.get(s)?;
                    Some(PkgInfo::new(
                        s,
                        vec![],
                        provides.clone(),
                        Some(s.clone()),
                        "rawhide",
                    ))
                })
                .collect())
        }
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(b, s)| (b.to_string(), s.to_string()))
            .collect()
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
        // almanah links libical but only BuildRequires
        // evolution-data-server's development files, which bring
        // libical-glib's in.
        r.brs
            .insert("almanah".into(), strs(&["pkgconfig(libecal-2.0)"]));
        r.available = vec![
            bin("cmake", "cmake", &["cmake = 3.31.6-1.fc46"], &[]),
            bin(
                "libical-devel",
                "libical",
                &["libical-devel = 3.0.20-11.fc46"],
                &["libical"],
            ),
            bin(
                "libical",
                "libical",
                &["libical = 3.0.20-11.fc46", "libical.so.3()(64bit)"],
                &[],
            ),
            bin("gtk4-devel", "gtk4", &["pkgconfig(gtk4) = 4.20.1"], &[]),
            bin(
                "libical-glib-devel",
                "libical",
                &["pkgconfig(libical-glib) = 3.0.20"],
                &[],
            ),
            bin(
                "evolution-data-server-devel",
                "evolution-data-server",
                &["pkgconfig(libecal-2.0) = 3.62.1"],
                &["pkgconfig(libical-glib)"],
            ),
            bin("libxslt", "libxslt", &["libxslt = 1.1.43-1.fc46"], &[]),
        ];
        r.subpkgs.insert(
            "kdepimlibs".into(),
            strs(&["kdepimlibs", "kdepimlibs-akonadi", "kdepimlibs-devel"]),
        );
        r.requiring
            .insert("kdepimlibs-akonadi".into(), strs(&["kuser", "kdepimlibs"]));

        // Development packages bringing libical's headers along, and
        // what builds against them.
        r.bin_requiring.insert(
            "libical-devel".into(),
            pairs(&[
                ("libical-glib-devel", "libical"),
                ("kf6-kcalendarcore-devel", "kf6-kcalendarcore"),
            ]),
        );
        r.bin_requiring.insert(
            "libical-glib-devel".into(),
            pairs(&[("evolution-data-server-devel", "evolution-data-server")]),
        );
        r.bin_requiring.insert(
            "evolution-data-server-devel".into(),
            pairs(&[
                ("folks-devel", "folks"),
                // A runtime package requiring it is not followed.
                ("evolution-data-server-ui", "evolution-data-server"),
            ]),
        );
        r.bin_requiring.insert(
            "kf6-kcalendarcore-devel".into(),
            pairs(&[("kf6-akonadi-calendar-devel", "akonadi-calendar")]),
        );
        r.building.insert(
            "evolution-data-server-devel".into(),
            // almanah links libical; folks is rebuilt in the side tag.
            strs(&["almanah", "folks", "gnome-phone-manager", "libreoffice"]),
        );
        r.building
            .insert("folks-devel".into(), strs(&["bubblemail", "libreoffice"]));
        r.building.insert(
            "kf6-kcalendarcore-devel".into(),
            strs(&["akonadi-calendar", "korganizer"]),
        );
        r.building
            .insert("kf6-akonadi-calendar-devel".into(), strs(&["korganizer"]));
        r
    }

    /// File dependencies are answered one by one, and a provider's
    /// Provides never list its files, so the recorded repository
    /// answers `/usr/bin/xsltproc` by name.
    impl Recorded {
        fn with_file(mut self, path: &str, provider: &str) -> Self {
            self.available.push(bin(provider, provider, &[path], &[]));
            self
        }
    }

    /// The libical side tag. evolution-data-server's rebuild keeps its
    /// SONAMEs; the other builds' binaries are not recorded, so they
    /// are not compared.
    fn libical_side_tag() -> SideTag {
        SideTag {
            name: "f46-build-side-158330".into(),
            nvrs: strs(SIDE_TAG),
            sonames: BTreeMap::from([(
                "evolution-data-server".to_string(),
                BTreeSet::from(["libecal-2.0.so.3()(64bit)".to_string()]),
            )]),
        }
    }

    fn libical_plan() -> RebuildPlan {
        let mut q = libical().with_file("/usr/bin/xsltproc", "libxslt");
        q.provides.insert(
            "evolution-data-server".into(),
            strs(&[
                "libecal-2.0.so.3()(64bit)",
                "pkgconfig(libecal-2.0) = 3.62.1",
            ]),
        );
        plan("libical", "rawhide", Some(&libical_side_tag()), &q, false).unwrap()
    }

    fn names(ds: &[&Dependent]) -> Vec<String> {
        ds.iter().map(|d| d.package.clone()).collect()
    }

    fn unbuildable(p: &RebuildPlan, package: &str) -> bool {
        p.unbuildable.iter().any(|u| u.package == package)
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
        // None of them needs another in its buildroot: one round.
        assert_eq!(p.rounds.len(), 1, "{:?}", p.rounds);
        assert!(p.waits_for.is_empty() && p.blocked_by.is_empty());
        assert_eq!(
            p.rounds[0],
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
        assert!(!unbuildable(&p, "kmymoney"));
        assert!(!unbuildable(&p, "phosh"));
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
        assert!(!p.rounds.concat().contains(&"cyrus-timezones".to_string()));
    }

    #[test]
    fn without_a_side_tag_every_dependent_needs_a_rebuild() {
        let q = libical().with_file("/usr/bin/xsltproc", "libxslt");
        let p = plan("libical", "rawhide", None, &q, false).unwrap();
        // 26 runtime dependents, less kdepimlibs, which cannot build.
        assert_eq!(p.rounds.concat().len(), 25);
        assert!(p.rebuilt_not_needed.is_empty());
        assert!(render(&p).contains("## Needs a rebuild (25)"));
    }

    #[test]
    fn a_dependent_not_declaring_the_library_says_how_it_gets_it() {
        let p = libical_plan();
        let reach = |pkg: &str| {
            p.dependents
                .iter()
                .find(|d| d.package == pkg)
                .map(|d| d.reach.clone())
                .unwrap()
        };
        assert_eq!(reach("asterisk"), Reach::Declared);
        assert_eq!(
            reach("almanah"),
            Reach::Through {
                path: strs(&[
                    "pkgconfig(libecal-2.0)",
                    "evolution-data-server-devel",
                    "libical-glib-devel",
                ])
            }
        );
        // No BuildRequires recorded for bluez: nothing leads to libical.
        assert_eq!(reach("bluez"), Reach::Unexplained);
        let out = render(&p);
        for want in [
            "## Linking libical without a BuildRequires on it",
            "- almanah: `pkgconfig(libecal-2.0)` → evolution-data-server-devel → \
             libical-glib-devel (rebuilt)",
            "- bluez: none of its BuildRequires leads to libical",
        ] {
            assert!(out.contains(want), "missing {want:?} in:\n{out}");
        }
    }

    #[test]
    fn packages_that_may_compile_the_headers_in_are_grouped() {
        let p = libical_plan();
        let groups: Vec<&str> = p
            .header_exposed
            .iter()
            .map(|e| e.through.as_str())
            .collect();
        assert_eq!(
            groups,
            ["evolution-data-server-devel", "kf6-kcalendarcore-devel"]
        );
        let eds = &p.header_exposed[0];
        assert_eq!(eds.further, strs(&["folks-devel"]));
        // almanah (links libical) and folks (rebuilt) are settled.
        assert_eq!(
            eds.packages.keys().cloned().collect::<Vec<_>>(),
            strs(&["bubblemail", "gnome-phone-manager", "libreoffice"])
        );
        assert_eq!(
            eds.packages["libreoffice"],
            strs(&["evolution-data-server-devel", "folks-devel"])
        );
        let kde = &p.header_exposed[1];
        assert_eq!(kde.further, strs(&["kf6-akonadi-calendar-devel"]));
        assert_eq!(
            kde.packages["korganizer"],
            strs(&["kf6-kcalendarcore-devel", "kf6-akonadi-calendar-devel"])
        );
        let out = render(&p);
        for want in [
            "## May compile in libical's headers (5)",
            "### Through evolution-data-server-devel (3)",
            "And the development packages requiring it: folks-devel",
            "- gnome-phone-manager\n",
            "- bubblemail (folks-devel)",
            "### Through kf6-kcalendarcore-devel (2)",
        ] {
            assert!(out.contains(want), "missing {want:?} in:\n{out}");
        }
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
            "None of these needs another in its buildroot, so they can all be built at once:",
            "## Cannot be rebuilt (1)",
            "- kdepimlibs: `pkgconfig(QJson)`. Once the new libical lands, these become \
             uninstallable with it: kuser",
            "## Already rebuilt in f46-build-side-158330 (12)",
            "- gnome-shell (`gnome-shell-51.1-2.fc46`)",
            "## Build-time only (1)",
            "## Rebuilt but not using libical (13)",
            "No other rebuild in the side tag changed a SONAME of its own",
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
        assert_eq!(shell["reach"]["kind"], "unexplained");
        let almanah = v["dependents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["package"] == "almanah")
            .unwrap();
        assert_eq!(almanah["reach"]["kind"], "through");
        assert_eq!(almanah["reach"]["path"][1], "evolution-data-server-devel");
        assert_eq!(shell["rebuilt"], "gnome-shell-51.1-2.fc46");
        assert_eq!(v["unbuildable"][0]["stranded"][0], "kuser");
    }

    /// A library `libfoo` whose dependents reach each other through
    /// their buildroots, the way gnome-shell reached bluez through
    /// gnome-bluetooth during the libical 4 rebuild.
    fn chain() -> Recorded {
        let mut r = Recorded::default();
        r.provides.insert(
            "libfoo".into(),
            strs(&["libfoo.so.1()(64bit)", "libfoo-devel = 1.0-1.fc46"]),
        );
        r.requiring.insert(
            "libfoo.so.1()(64bit)".into(),
            strs(&[
                "bluez",
                "gnome-shell",
                "app",
                "loop-a",
                "loop-b",
                "kuser",
                "oldlib",
            ]),
        );
        // gnome-shell builds against gnome-bluetooth, which is not
        // rebuilt (it does not link libfoo) but pulls in bluez, which
        // does.
        r.brs
            .insert("gnome-shell".into(), strs(&["gnome-bluetooth-libs-devel"]));
        r.brs.insert("bluez".into(), strs(&["libfoo-devel"]));
        // app needs gnome-shell's -devel, two hops from libfoo.
        r.brs.insert("app".into(), strs(&["gnome-shell-devel"]));
        // loop-a and loop-b need each other's rebuilds.
        r.brs.insert("loop-a".into(), strs(&["loop-b-devel"]));
        r.brs.insert("loop-b".into(), strs(&["loop-a-devel"]));
        // kuser needs oldlib, whose own BuildRequires are gone.
        r.brs.insert("kuser".into(), strs(&["oldlib-devel"]));
        r.brs.insert("oldlib".into(), strs(&["pkgconfig(QJson)"]));
        r.available = vec![
            bin("libfoo-devel", "libfoo", &[], &["libfoo"]),
            bin("libfoo", "libfoo", &["libfoo.so.1()(64bit)"], &[]),
            bin(
                "gnome-bluetooth-libs-devel",
                "gnome-bluetooth",
                &[],
                &["gnome-bluetooth-libs"],
            ),
            bin("gnome-bluetooth-libs", "gnome-bluetooth", &[], &["bluez"]),
            bin("bluez", "bluez", &[], &["libfoo.so.1()(64bit)"]),
            bin("gnome-shell-devel", "gnome-shell", &[], &["gnome-shell"]),
            bin("gnome-shell", "gnome-shell", &[], &["libfoo.so.1()(64bit)"]),
            bin("loop-a-devel", "loop-a", &[], &["loop-a"]),
            bin("loop-a", "loop-a", &[], &["libfoo.so.1()(64bit)"]),
            bin("loop-b-devel", "loop-b", &[], &["loop-b"]),
            bin("loop-b", "loop-b", &[], &["libfoo.so.1()(64bit)"]),
            bin("oldlib-devel", "oldlib", &[], &["oldlib"]),
            bin("oldlib", "oldlib", &[], &["libfoo.so.1()(64bit)"]),
        ];
        r.subpkgs
            .insert("oldlib".into(), strs(&["oldlib", "oldlib-devel"]));
        r.requiring.insert("oldlib".into(), strs(&["kuser"]));
        r
    }

    fn chain_plan() -> RebuildPlan {
        plan("libfoo", "rawhide", None, &chain(), false).unwrap()
    }

    /// The chain, with bluez rebuilt in a side tag that dropped
    /// `libbluetooth.so.3`, which blueman links without linking libfoo.
    fn chain_with_soname_change() -> RebuildPlan {
        let mut q = chain();
        q.provides.insert(
            "bluez".into(),
            strs(&["libbluetooth.so.3()(64bit)", "bluez = 5.87-1.fc46"]),
        );
        q.requiring.insert(
            "libbluetooth.so.3()(64bit)".into(),
            strs(&["blueman", "bluez"]),
        );
        let st = SideTag {
            name: "f46-build-side-1".into(),
            nvrs: strs(&["bluez-5.87-2.fc46"]),
            sonames: BTreeMap::from([(
                "bluez".to_string(),
                BTreeSet::from(["libbluetooth.so.4()(64bit)".to_string()]),
            )]),
        };
        plan("libfoo", "rawhide", Some(&st), &q, false).unwrap()
    }

    #[test]
    fn a_rebuild_dropping_a_soname_brings_in_what_links_it() {
        let p = chain_with_soname_change();
        assert_eq!(p.soname_changes.len(), 1, "{:?}", p.soname_changes);
        let c = &p.soname_changes[0];
        assert_eq!(c.package, "bluez");
        assert_eq!(c.removed, strs(&["libbluetooth.so.3()(64bit)"]));
        assert_eq!(c.dependents, strs(&["blueman"]));
        let blueman = p
            .dependents
            .iter()
            .find(|d| d.package == "blueman")
            .unwrap();
        assert_eq!(
            blueman.reach,
            Reach::Relinks {
                package: "bluez".into()
            }
        );
        assert!(p.rounds.concat().contains(&"blueman".to_string()));
        // It is not a package linking libfoo without declaring it.
        assert!(!names(&p.undeclared()).contains(&"blueman".to_string()));
        let out = render(&p);
        for want in [
            "## Rebuilds that changed a SONAME of their own (1)",
            "- bluez: drops `libbluetooth.so.3()(64bit)`; linked by blueman",
        ] {
            assert!(out.contains(want), "missing {want:?} in:\n{out}");
        }
    }

    #[test]
    fn a_rebuild_keeping_its_sonames_adds_nothing() {
        let p = libical_plan();
        assert!(p.soname_changes.is_empty(), "{:?}", p.soname_changes);
        // Only evolution-data-server's binaries were recorded; every
        // other build is reported as not compared, not as unchanged.
        assert!(
            !p.not_compared
                .contains(&"evolution-data-server".to_string())
        );
        assert!(!p.not_compared.contains(&"libical".to_string()));
        assert!(p.not_compared.contains(&"gnome-shell".to_string()));
        let out = render(&p);
        assert!(out.contains("Not checked for SONAME changes"), "{out}");
        assert!(
            out.contains("No other rebuild in the side tag changed a SONAME of its own"),
            "{out}"
        );
    }

    #[test]
    fn side_tag_sonames_come_from_the_tagged_build_only() {
        let info = |name: &str, src: &str, vr: (&str, &str), provides: &[&str]| {
            let mut pi = bin(name, src, provides, &[]);
            pi.version = Some(vr.0.into());
            pi.release = Some(vr.1.into());
            pi
        };
        let infos = vec![
            info(
                "bluez-libs",
                "bluez",
                ("5.87", "2.fc46"),
                &[
                    "libbluetooth.so.4()(64bit)",
                    "libbluetooth.so.4(BLUEZ_4)(64bit)",
                ],
            ),
            // Inherited from the branch: not the tagged build.
            info(
                "bluez-libs",
                "bluez",
                ("5.87", "1.fc46"),
                &["libbluetooth.so.3()(64bit)"],
            ),
            info("bluez-obexd", "bluez", ("5.87", "2.fc46"), &[]),
            // Not tagged at all.
            info(
                "gtk4",
                "gtk4",
                ("4.20.1", "1.fc46"),
                &["libgtk-4.so.1()(64bit)"],
            ),
        ];
        let got = sonames_by_source(&strs(&["bluez-5.87-2.fc46"]), &infos);
        assert_eq!(
            got,
            BTreeMap::from([(
                "bluez".to_string(),
                BTreeSet::from(["libbluetooth.so.4()(64bit)".to_string()])
            )])
        );
    }

    #[test]
    fn a_buildroot_reaches_a_stale_binary_through_a_package_not_rebuilt() {
        let p = chain_plan();
        assert_eq!(p.waits_for["gnome-shell"], strs(&["bluez"]));
        let round = |pkg: &str| p.rounds.iter().position(|r| r.iter().any(|x| x == pkg));
        assert!(round("bluez") < round("gnome-shell"), "{:?}", p.rounds);
        assert!(round("gnome-shell") < round("app"), "{:?}", p.rounds);
    }

    #[test]
    fn a_package_needing_one_that_cannot_be_rebuilt_is_blocked() {
        let p = chain_plan();
        assert!(unbuildable(&p, "oldlib"));
        assert_eq!(p.blocked_by["kuser"], strs(&["oldlib"]));
        assert!(!p.rounds.concat().contains(&"kuser".to_string()));
        assert!(render(&p).contains("- kuser: needs oldlib"));
    }

    #[test]
    fn packages_needing_each_other_are_one_round_and_reported_as_a_cycle() {
        let p = chain_plan();
        assert_eq!(p.cycles, vec![strs(&["loop-a", "loop-b"])]);
        let round = |pkg: &str| p.rounds.iter().position(|r| r.iter().any(|x| x == pkg));
        assert_eq!(round("loop-a"), round("loop-b"));
        assert!(render(&p).contains("Cycle: loop-a, loop-b"));
    }

    #[test]
    fn rounds_are_rendered_with_what_each_waits_for() {
        let out = render(&chain_plan());
        assert!(out.contains("### Round 1"), "{out}");
        assert!(out.contains("- gnome-shell (after bluez)"), "{out}");
    }
}
