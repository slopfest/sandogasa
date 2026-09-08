// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use sandogasa_pkg_health::checks::dependency_health::{self, DepFacts};
use sandogasa_pkg_health::{Context, CostTier, HealthReport, duration, registry::default_registry};

#[derive(Parser)]
#[command(
    about,
    long_about = None,
    max_term_width = 80,
    version = sandogasa_cli::version!(),
    before_help = sandogasa_cli::banner!()
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List registered health checks and their cost tiers.
    Checks,
    /// Run health checks against an inventory.
    Run(RunArgs),
    /// Display a previously-generated health report without
    /// re-running any checks.
    Show(ShowArgs),
}

#[derive(clap::Args)]
struct RunArgs {
    /// Path to inventory TOML file (default: the workspace's `owned`
    /// inventory when -w is given).
    #[arg(
        short,
        long,
        value_name = "PATH",
        required_unless_present = "workspace"
    )]
    inventory: Option<String>,

    /// A poi-tracker workspace file (kondo.toml): its `owned` inventory
    /// is the default -i, and the saved graphs of its closures for
    /// --branch feed the dependency_health check.
    #[arg(short, long, value_name = "PATH")]
    workspace: Option<String>,

    /// The branch whose closures -w reads graphs from. Graphs of
    /// different branches are never merged: a binary's source package
    /// differs between them (CentOS Stream 9 ships rust-srpm-macros
    /// as its own package; rawhide has it in cargo-rpm-macros).
    #[arg(long, value_name = "BRANCH", default_value = "rawhide")]
    branch: String,

    /// A dependency graph saved by `poi-tracker deps --graph`
    /// (repeatable); with one, each package's dependencies are
    /// checked too and dependency_health is computed.
    #[arg(long, value_name = "PATH")]
    graph: Vec<String>,

    /// How many levels of dependencies to walk for dependency_health:
    /// 1 is the direct ones. Through build dependencies nearly every
    /// package reaches the whole toolchain within a few levels.
    #[arg(long, value_name = "N", default_value_t = 1)]
    dependency_depth: usize,

    /// Path to report TOML file (read if exists, written after run).
    #[arg(short, long, value_name = "PATH")]
    output: String,

    /// Run only these checks (repeatable).
    #[arg(long = "check", value_name = "ID")]
    checks: Vec<String>,

    /// Run all cheap-tier checks.
    #[arg(long, conflicts_with_all = ["medium", "expensive", "all"])]
    cheap: bool,

    /// Run all medium-tier checks.
    #[arg(long, conflicts_with_all = ["cheap", "expensive", "all"])]
    medium: bool,

    /// Run all expensive-tier checks.
    #[arg(long, conflicts_with_all = ["cheap", "medium", "all"])]
    expensive: bool,

    /// Run all checks regardless of tier.
    #[arg(long, conflicts_with_all = ["cheap", "medium", "expensive"])]
    all: bool,

    /// Re-run any selected check whose stored result is older than
    /// this duration (e.g. "7d", "24h").
    #[arg(long, value_name = "DURATION")]
    max_age: Option<String>,

    /// Limit to specific packages (repeatable).
    #[arg(long = "package", value_name = "NAME")]
    packages: Vec<String>,

    /// Output summary as JSON.
    #[arg(long)]
    json: bool,

    /// Fedora version(s) for FTBFS / FTI tracker lookup (CSV or
    /// repeated). Rawhide trackers are always included.
    #[arg(long = "fedora-version", value_name = "N", value_delimiter = ',')]
    fedora_versions: Vec<u32>,

    /// EPEL version(s) for FTBFS / FTI tracker lookup (CSV or
    /// repeated).
    #[arg(long = "epel-version", value_name = "N", value_delimiter = ',')]
    epel_versions: Vec<u32>,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(clap::Args)]
struct ShowArgs {
    /// Path to an existing report TOML file.
    report: String,

    /// Limit to specific packages (repeatable).
    #[arg(long = "package", value_name = "NAME")]
    packages: Vec<String>,

    /// Output as JSON instead of human-readable.
    #[arg(long)]
    json: bool,
}

/// Outcome of running a single (package, check, variant) work item.
enum PackageOutcome {
    Fresh,
    Ran {
        key: String,
        data: serde_json::Value,
    },
    Failed,
}

/// Sort and deduplicate a version list, warning on duplicates.
/// The sorted pass collects the repeats (each named once, in
/// ascending order) as it drops them.
fn dedup_versions(versions: &[u32], label: &str) -> Vec<u32> {
    let mut sorted: Vec<u32> = versions.to_vec();
    sorted.sort_unstable();
    let mut unique: Vec<u32> = Vec::with_capacity(sorted.len());
    let mut dups: Vec<u32> = Vec::new();
    for v in sorted {
        if unique.last() == Some(&v) {
            if dups.last() != Some(&v) {
                dups.push(v);
            }
        } else {
            unique.push(v);
        }
    }
    if !dups.is_empty() {
        eprintln!(
            "warning: duplicate --{label}-version value(s) ignored: {}",
            dups.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    unique
}

fn main() -> ExitCode {
    sandogasa_cli::init();
    let cli = sandogasa_cli::parse_with_defaults::<Cli>(env!("CARGO_PKG_NAME"));
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to build runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async {
        match cli.command {
            Command::Checks => cmd_checks(),
            Command::Run(args) => cmd_run(&args).await,
            Command::Show(args) => cmd_show(&args),
        }
    })
}

fn cmd_checks() -> ExitCode {
    let reg = default_registry();
    println!("Available health checks:\n");
    for check in reg.all() {
        println!(
            "  {:20} [{:?}] — {}",
            check.id(),
            check.cost_tier(),
            check.description()
        );
    }
    ExitCode::SUCCESS
}

fn cmd_show(args: &ShowArgs) -> ExitCode {
    let report = match HealthReport::load(&args.report) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("JSON serialization failed")
        );
        return ExitCode::SUCCESS;
    }

    let reg = default_registry();
    // Default: every package in the report. --package filters down.
    let packages: Vec<&str> = if args.packages.is_empty() {
        report.package.keys().map(|s| s.as_str()).collect()
    } else {
        args.packages.iter().map(|s| s.as_str()).collect()
    };
    print_summary(&report, &reg, &packages);
    ExitCode::SUCCESS
}

async fn cmd_run(args: &RunArgs) -> ExitCode {
    // The workspace names the inventory and the graphs; flags win.
    let workspace = match args.workspace.as_deref() {
        Some(path) => match sandogasa_inventory::workspace::Workspace::find(Some(path)) {
            Ok(Some((ws, _))) => Some(ws),
            Ok(None) => unreachable!("an explicit workspace path is always looked up"),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };
    let inventory_path = match (&args.inventory, &workspace) {
        (Some(p), _) => p.clone(),
        (None, Some(ws)) => match &ws.owned {
            Some(owned) => ws.resolve(owned),
            None => {
                eprintln!("error: the workspace names no `owned` inventory; pass -i");
                return ExitCode::FAILURE;
            }
        },
        (None, None) => unreachable!("clap requires -i or -w"),
    };
    let inventory = match sandogasa_inventory::load(&inventory_path) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    // The graphs to read: --graph as given, and the workspace's closures
    // for --branch. One branch only — a source name from another
    // branch's graph would be looked up in the wrong dist-git.
    let mut graph_paths: Vec<String> = args.graph.clone();
    if let Some(ws) = &workspace {
        let on_branch: Vec<&sandogasa_inventory::workspace::Closure> = ws
            .closures
            .iter()
            .filter(|c| c.branch == args.branch)
            .collect();
        if on_branch.is_empty() && args.graph.is_empty() {
            eprintln!(
                "error: the workspace has no closure for branch {}; it has: {}",
                args.branch,
                ws.closures
                    .iter()
                    .map(|c| format!("{} ({})", c.name, c.branch))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return ExitCode::FAILURE;
        }
        graph_paths.extend(
            on_branch
                .iter()
                .filter_map(|c| c.graph.as_deref().map(|g| ws.resolve(g))),
        );
    }
    // A graph is a snapshot of the repositories; say how old.
    for path in &graph_paths {
        if let Some(days) = file_age_days(path) {
            if days > 30 {
                eprintln!(
                    "note: {path} is {days} days old; the repositories have moved on — regenerate \
                     it with poi-tracker deps --graph"
                );
            } else if args.verbose {
                eprintln!("[pkg-health] {path}: {days} day(s) old");
            }
        }
    }
    let mut graph: Option<sandogasa_closure::DepsGraph> = None;
    for path in &graph_paths {
        match sandogasa_closure::DepsGraph::load(path) {
            Ok(g) => match &mut graph {
                Some(all) => all.merge(g),
                None => graph = Some(g),
            },
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    let reg = default_registry();

    // Load existing report or create fresh.
    let mut report = if std::path::Path::new(&args.output).exists() {
        match HealthReport::load(&args.output) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        HealthReport::new(&inventory.inventory.name)
    };

    // Determine which checks to run. A tier flag (mutually
    // exclusive with the others via clap) selects that whole
    // tier and wins over an explicit --check list; with no flag
    // at all the default is the cheap tier.
    let tier = if args.cheap {
        Some(CostTier::Cheap)
    } else if args.medium {
        Some(CostTier::Medium)
    } else if args.expensive {
        Some(CostTier::Expensive)
    } else if args.checks.is_empty() {
        Some(CostTier::Cheap)
    } else {
        None
    };
    let selected_ids: Vec<&str> = if args.all {
        reg.all().map(|c| c.id()).collect()
    } else if let Some(tier) = tier {
        reg.by_tier(tier).map(|c| c.id()).collect()
    } else {
        args.checks.iter().map(|s| s.as_str()).collect()
    };

    // The checks that read the workspace have nothing to read without
    // one; leave them out rather than fail each package on them.
    let selected_ids: Vec<&str> = selected_ids
        .into_iter()
        .filter(|id| {
            let needs = reg.get(id).is_some_and(|c| c.needs_workspace());
            if needs && workspace.is_none() && args.verbose {
                eprintln!("[pkg-health] skipping {id}: needs a workspace (-w)");
            }
            !needs || workspace.is_some()
        })
        .collect();

    if args.verbose {
        eprintln!("[pkg-health] running checks: {}", selected_ids.join(", "));
    }

    // Determine which packages to check.
    let packages: Vec<&str> = if args.packages.is_empty() {
        inventory.package.iter().map(|p| p.name.as_str()).collect()
    } else {
        args.packages.iter().map(|s| s.as_str()).collect()
    };

    // With a graph, each package's dependencies are read off it: the
    // direct ones and the transitive closure, at the source level. The
    // dependencies not in the inventory are checked too — the two
    // facts the dependency reading is built from — so the reading can
    // be computed from stored results once every check has run.
    let dependencies = sandogasa_closure_deps(graph.as_ref(), &packages, args.dependency_depth);
    let inventory_set: std::collections::BTreeSet<&str> = packages.iter().copied().collect();
    let dependency_packages: Vec<String> = dependencies
        .values()
        .flat_map(|(d, t)| d.iter().map(|(n, _)| n).chain(t.iter()))
        .filter(|p| !inventory_set.contains(p.as_str()))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    if args.verbose {
        eprintln!(
            "[pkg-health] {} package(s) to check{}",
            packages.len(),
            if dependency_packages.is_empty() {
                String::new()
            } else {
                format!(
                    ", and {} dependenc(y|ies) of theirs",
                    dependency_packages.len()
                )
            }
        );
    }

    // Parse --max-age if given.
    let max_age = match args.max_age.as_deref() {
        Some(s) => match duration::parse(s) {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };

    let fedora_versions = dedup_versions(&args.fedora_versions, "fedora");
    let epel_versions = dedup_versions(&args.epel_versions, "epel");

    let mut ctx = Context::new(&fedora_versions, &epel_versions, args.verbose).await;
    // What the workspace and its graph say, for the checks that read
    // them: the essential inventories' packages, the retired ones kept
    // knowingly, who depends on whom.
    if let Some(ws) = &workspace {
        let mut facts = sandogasa_pkg_health::context::WorkspaceFacts {
            user: ws.user.clone(),
            inventory: inventory.package.iter().map(|p| p.name.clone()).collect(),
            ..Default::default()
        };
        let names = |path: &str| -> Result<Vec<String>, String> {
            sandogasa_inventory::load(path)
                .map(|inv| inv.package.into_iter().map(|p| p.name).collect())
        };
        for path in ws.essential() {
            match names(&ws.resolve(&path)) {
                Ok(n) => facts.essential.extend(n),
                Err(e) => eprintln!("warning: {e}"),
            }
        }
        for path in &ws.retired {
            match names(&ws.resolve(path)) {
                Ok(n) => facts.retired_kept.extend(n),
                Err(e) => eprintln!("warning: {e}"),
            }
        }
        if let Some(g) = &graph {
            facts.add_graph(g);
        }
        ctx.workspace = Some(std::sync::Arc::new(facts));
    }

    // The work: every selected check over the inventory's packages —
    // dependency_health excepted, it is computed afterwards — and the
    // two facts the dependency reading needs over the dependencies,
    // bug_count on rawhide only to bound the cost.
    const DEP_CHECKS: &[(&str, Option<&str>)] =
        &[("maintainer_count", None), ("bug_count", Some("rawhide"))];
    let mut work: Work<'_> = Vec::new();
    for pkg in &packages {
        let mut items = Vec::new();
        for check_id in &selected_ids {
            if *check_id == "dependency_health" {
                continue;
            }
            let Some(check) = reg.get(check_id) else {
                eprintln!("warning: unknown check '{check_id}'");
                continue;
            };
            items.extend(check.variants(&ctx).into_iter().map(|v| (*check_id, v)));
        }
        work.push((pkg, items));
    }
    for pkg in &dependency_packages {
        work.push((
            pkg.as_str(),
            DEP_CHECKS
                .iter()
                .map(|(c, v)| (*c, v.map(str::to_string)))
                .collect(),
        ));
    }
    let total = work.len();
    let width = total.to_string().len();
    let completed = std::sync::atomic::AtomicUsize::new(0);

    // Each (package, check_id, variant) work item produces one of:
    // - PackageOutcome::Fresh: skipped per --max-age
    // - PackageOutcome::Ran { key, data }: needs to be written to report
    // - PackageOutcome::Failed: logged, counted
    use rayon::prelude::*;
    let outcomes: Vec<(String, PackageOutcome)> = work
        .par_iter()
        .flat_map_iter(|(pkg, checks)| {
            let i = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if args.verbose {
                eprintln!("[pkg-health] [{i:>width$}/{total}] {pkg}", width = width,);
            }
            let mut items: Vec<(String, PackageOutcome)> = Vec::new();
            for (check_id, variant) in checks {
                let Some(check) = reg.get(check_id) else {
                    continue;
                };
                {
                    let key = sandogasa_pkg_health::entry_key(check_id, variant.as_deref());

                    if let Some(age) = max_age
                        && !report.is_stale(pkg, &key, age)
                    {
                        items.push((pkg.to_string(), PackageOutcome::Fresh));
                        continue;
                    }

                    match check.run(pkg, variant.as_deref(), &ctx) {
                        Ok(result) => items.push((
                            pkg.to_string(),
                            PackageOutcome::Ran {
                                key,
                                data: result.data,
                            },
                        )),
                        Err(e) => {
                            eprintln!("warning: {pkg}: {key}: {e}");
                            items.push((pkg.to_string(), PackageOutcome::Failed));
                        }
                    }
                }
            }
            items
        })
        .collect();

    let mut ran = 0usize;
    let mut fresh = 0usize;
    let mut failed = 0usize;
    for (pkg, outcome) in outcomes {
        match outcome {
            PackageOutcome::Fresh => fresh += 1,
            PackageOutcome::Ran { key, data } => {
                report.update(&pkg, &key, data);
                ran += 1;
            }
            PackageOutcome::Failed => failed += 1,
        }
    }

    // The dependency reading, from what is now stored about each
    // dependency; a run without a graph leaves any earlier reading as
    // it was.
    // A graph is a snapshot: a dependency it names may have been
    // retired since (or, in a graph of the wrong branch, never existed
    // here — a first cut merged CentOS Stream 9's rust-srpm-macros into
    // rawhide's reading). Before a dependency is reported as needing
    // attention, confirm the branch still has a package of that name;
    // one that is gone is listed as such rather than counted.
    let mut gone_cache: std::collections::BTreeMap<String, bool> =
        std::collections::BTreeMap::new();
    let fedrq_available = dependencies.is_empty()
        || sandogasa_cli::require_tools(&[("fedrq", "sudo dnf install fedrq", Some("--version"))])
            .is_ok();
    if !fedrq_available {
        eprintln!(
            "note: fedrq is not installed, so dependencies the graph names cannot be checked \
             against the branch; a retired one would still be reported"
        );
    }
    for (pkg, (direct, transitive)) in &dependencies {
        let mut facts: std::collections::BTreeMap<String, DepFacts> = direct
            .iter()
            .map(|(n, _)| n)
            .chain(transitive.iter())
            .filter_map(|d| DepFacts::from_report(&report, d).map(|f| (d.clone(), f)))
            .collect();
        if fedrq_available {
            for (name, f) in facts.iter_mut() {
                if !f.reasons().is_empty() {
                    let gone = *gone_cache
                        .entry(name.clone())
                        .or_insert_with(|| !on_branch(name, &args.branch));
                    f.gone = gone;
                }
            }
        }
        report.update(
            pkg,
            "dependency_health",
            dependency_health::aggregate(direct, transitive, &facts),
        );
        ran += 1;
    }

    if let Err(e) = report.save(&args.output) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("JSON serialization failed")
        );
    } else {
        print_summary(&report, &reg, &packages);
        eprintln!(
            "\nRan {ran} check(s), {fresh} fresh (skipped), {failed} failed, \
             wrote report to {}",
            args.output
        );
    }

    ExitCode::SUCCESS
}

/// Print a human-readable per-package summary using each check's
/// Whether `branch` still has a *source* package named `name`, per
/// fedrq. The source is what the facts are about (dist-git ACLs, bugs),
/// and a binary of that name may well live on under another source —
/// rawhide's `rust-srpm-macros` binary is cargo-rpm-macros'. `true` on
/// a fedrq failure: a package cannot be called gone on no evidence.
fn on_branch(name: &str, branch: &str) -> bool {
    let out = std::process::Command::new("fedrq")
        .args(["pkgs", "-b", branch, "--src", "-F", "name", "--", name])
        .output();
    match out {
        Ok(o) if o.status.success() => !String::from_utf8_lossy(&o.stdout).trim().is_empty(),
        _ => true,
    }
}

/// How many days ago `path` was last written, when the file says.
fn file_age_days(path: &str) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.elapsed().ok()?.as_secs() / 86_400)
}

/// The checks to run per package: `(check id, variant)` pairs.
type Work<'a> = Vec<(&'a str, Vec<(&'a str, Option<String>)>)>;

/// Each package's dependencies off the graph, `depth` levels deep:
/// the direct ones with their kind, then everything reached beyond
/// them within the depth. Empty without a graph, and a package the
/// graph never saw has no entry.
fn sandogasa_closure_deps(
    graph: Option<&sandogasa_closure::DepsGraph>,
    packages: &[&str],
    depth: usize,
) -> std::collections::BTreeMap<String, (Vec<dependency_health::Direct>, Vec<String>)> {
    let mut out = std::collections::BTreeMap::new();
    let Some(graph) = graph else {
        return out;
    };
    let deps = graph.dependencies_by_kind();
    for pkg in packages {
        let Some(direct) = deps.get(*pkg) else {
            continue;
        };
        let mut seen: std::collections::BTreeSet<&str> =
            direct.keys().map(String::as_str).collect();
        seen.insert(pkg);
        let mut frontier: Vec<&str> = direct.keys().map(String::as_str).collect();
        let mut transitive: Vec<String> = Vec::new();
        for _ in 1..depth.max(1) {
            let mut next_frontier = Vec::new();
            for cur in frontier {
                for next in deps.get(cur).into_iter().flat_map(|m| m.keys()) {
                    if seen.insert(next.as_str()) {
                        transitive.push(next.clone());
                        next_frontier.push(next.as_str());
                    }
                }
            }
            frontier = next_frontier;
        }
        transitive.sort();
        out.insert(
            pkg.to_string(),
            (
                direct
                    .iter()
                    .map(|(n, k)| (n.clone(), *k == sandogasa_closure::DepKind::Runtime))
                    .collect(),
                transitive,
            ),
        );
    }
    out
}

/// format_result override.
fn print_summary(report: &HealthReport, reg: &sandogasa_pkg_health::Registry, packages: &[&str]) {
    println!("Health summary ({})\n", report.report.inventory);
    for pkg in packages {
        let Some(pkg_report) = report.package.get(*pkg) else {
            continue;
        };
        if pkg_report.checks.is_empty() {
            continue;
        }
        println!("{pkg}:");
        for (key, entry) in &pkg_report.checks {
            let (check_id, _variant) = match key.split_once(':') {
                Some((a, b)) => (a, Some(b)),
                None => (key.as_str(), None),
            };
            let summary = reg.get(check_id).map_or_else(
                || serde_json::to_string(&entry.data).unwrap_or_default(),
                |c| c.format_result(&entry.data),
            );
            println!("  {key}: {summary}");
        }
    }
}

#[cfg(test)]
mod tests {
    /// The committed man page is generated from this CLI; see
    /// `sandogasa_cli::man` and `scripts/gen-man.sh`.
    #[test]
    fn man_page_matches_cli() {
        sandogasa_cli::man::check::<super::Cli>(
            concat!(env!("CARGO_MANIFEST_DIR"), "/man/sandogasa-pkg-health.1"),
            env!("CARGO_PKG_VERSION"),
        );
    }
}
