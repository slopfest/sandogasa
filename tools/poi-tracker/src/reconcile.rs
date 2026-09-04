// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `reconcile` — the maintenance loop as one command over the
//! workspace: for every closure, walk the keeps the graph has not
//! seen, recompute the derived inventory, and ask only the decisions
//! a program cannot make; then triage what nothing justifies.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Write as _};
use std::sync::atomic::Ordering::Relaxed;

use serde::Serialize;

use crate::workspace::{Closure, Workspace};
use crate::{dependents, deps, derive, kondo, unkeep};

/// What the user asked for.
pub struct Options<'a> {
    pub ws: &'a Workspace,
    /// Essential inventory promotions go to (default: the closure's
    /// first keeps file).
    pub into: Option<String>,
    /// Take every default without asking.
    pub yes: bool,
    pub json: bool,
    pub verbose: bool,
    /// Compute and report; write nothing (new keeps are listed, not
    /// walked).
    pub dry_run: bool,
}

/// What one closure's pass did.
#[derive(Debug, Default, Serialize)]
pub struct ClosureReport {
    pub name: String,
    pub external: bool,
    /// Why nothing was done, when nothing was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    /// Keeps the graph had never walked as roots — walked now.
    pub new_keeps: Vec<String>,
    /// Former keeps: graph roots that are neither kept, nor retired,
    /// nor reached from the keeps any more (fixpoint roots — owned
    /// packages the walk reached — are roots too, and stay reachable).
    pub dropped_keeps: Vec<String>,
    /// Packages that entered the derived inventory.
    pub derived_added: Vec<String>,
    /// Packages that left it.
    pub derived_removed: Vec<String>,
    /// Derived packages the user made keeps.
    pub promoted: Vec<String>,
    /// Keeps the user demoted to the derived inventory.
    pub demoted: Vec<String>,
    /// Packages added to the closure's walk-output inventory.
    pub closure_added: Vec<String>,
    pub capabilities_offline: usize,
    pub capabilities_online: usize,
}

/// The whole run, as `--json` prints it.
#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub closures: Vec<ClosureReport>,
    /// Owned packages no essential inventory justifies (not yet
    /// culled), for the triage; listed instead of prompted in JSON.
    pub triage_candidates: Vec<String>,
}

fn names_of(path: &str) -> Result<BTreeSet<String>, String> {
    Ok(sandogasa_inventory::load(path)?
        .package
        .into_iter()
        .map(|p| p.name)
        .collect())
}

fn read_graph(path: &str) -> Result<deps::DepsGraph, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("reading {path}: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parsing {path}: {e}"))
}

fn write_graph(path: &str, graph: &deps::DepsGraph) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(graph).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| format!("writing {path}: {e}"))
}

/// Add packages (with reasons) to an inventory file, creating it from
/// `meta_from`'s header when absent; returns the names actually added.
fn add_packages(
    path: &str,
    meta_from: &str,
    packages: &[(String, String)],
) -> Result<Vec<String>, String> {
    let mut inv = match std::path::Path::new(path).exists() {
        true => sandogasa_inventory::load(path)?,
        false => sandogasa_inventory::Inventory {
            inventory: sandogasa_inventory::load(meta_from)?.inventory,
            package: Vec::new(),
        },
    };
    let have: BTreeSet<String> = inv.package.iter().map(|p| p.name.clone()).collect();
    let mut added = Vec::new();
    for (name, reason) in packages {
        if have.contains(name) {
            continue;
        }
        inv.package.push(sandogasa_inventory::Package {
            name: name.clone(),
            reason: Some(reason.clone()),
            ..Default::default()
        });
        added.push(name.clone());
    }
    if !added.is_empty() {
        sandogasa_inventory::save(&inv, path)?;
    }
    Ok(added)
}

/// One answer at the prompt: Enter takes `default`; `a` takes the
/// default for the rest of the batch; `q` stops asking altogether.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Choice(char),
    AllDefault,
    Quit,
}

fn parse_answer(line: &str, default: char, choices: &[char]) -> Option<Answer> {
    let l = line.trim().to_ascii_lowercase();
    match l.as_str() {
        "" => Some(Answer::Choice(default)),
        "a" | "all" => Some(Answer::AllDefault),
        "q" | "quit" => Some(Answer::Quit),
        s => s
            .chars()
            .next()
            .filter(|c| s.len() == 1 && choices.contains(c))
            .map(Answer::Choice),
    }
}

fn ask(prompt: &str, default: char, choices: &[char]) -> Result<Answer, String> {
    loop {
        eprint!("{prompt} ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        if std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?
            == 0
        {
            return Ok(Answer::Quit);
        }
        if let Some(a) = parse_answer(&line, default, choices) {
            return Ok(a);
        }
        eprintln!(
            "  answer one of {} (Enter = {default}, a = default for all, q = quit)",
            choices
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join("/")
        );
    }
}

/// Walk `names` as new roots against the closure's repos, graph-first;
/// merges the edges into `graph` and returns the walk report.
fn walk_new(
    c: &Closure,
    graph: &mut deps::DepsGraph,
    names: &[String],
    owned: &BTreeSet<String>,
    verbose: bool,
) -> Result<(deps::DepsReport, usize, usize), String> {
    sandogasa_cli::require_tools(&[("fedrq", "sudo dnf install fedrq", Some("--version"))])?;
    let fedrq = sandogasa_fedrq::Fedrq {
        branch: Some(c.branch.clone()),
        repo: c.repo.clone(),
    };
    let query = deps::GraphBackedQuery::new(&fedrq, graph);
    let from: BTreeSet<String> = if c.from.is_empty() {
        ["epel".to_string()].into()
    } else {
        c.from.iter().cloned().collect()
    };
    let base: Vec<String> = if c.base_repo.is_empty() {
        vec!["fedrq-centos-stream-".to_string()]
    } else {
        c.base_repo.clone()
    };
    let (report, new_graph) = deps::walk(
        &query,
        names,
        &deps::WalkOpts {
            from: &from,
            base_prefixes: &base,
            build: !c.runtime_only,
            fixpoint_own: Some(owned),
            verbose,
        },
    )?;
    let (offline, online) = (query.offline.load(Relaxed), query.online.load(Relaxed));
    graph.merge(new_graph);
    Ok((report, offline, online))
}

/// One closure, to its fixpoint: promotions become new keeps and are
/// walked in the next round, until a round changes nothing.
fn reconcile_closure(
    opts: &Options,
    c: &Closure,
    owned: &BTreeSet<String>,
    retired: &BTreeSet<String>,
    interactive: bool,
) -> Result<ClosureReport, String> {
    let ws = opts.ws;
    let resolve = |p: &str| ws.dir.join(p).to_string_lossy().into_owned();
    let mut report = ClosureReport {
        name: c.name.clone(),
        external: c.external,
        ..Default::default()
    };
    let Some(graph_path) = c.graph.as_deref().map(resolve) else {
        report.skipped =
            Some("no graph in the workspace: run `poi-tracker --closure NAME deps` once".into());
        return Ok(report);
    };
    if !std::path::Path::new(&graph_path).exists() {
        report.skipped = Some(format!(
            "{graph_path} does not exist yet: run `poi-tracker --closure {} deps` once",
            c.name
        ));
        return Ok(report);
    }
    let mut graph = read_graph(&graph_path)?;
    let keeps_files: Vec<String> = c.keeps.iter().map(|p| resolve(p)).collect();
    let into = opts.into.clone().unwrap_or_else(|| keeps_files[0].clone());
    let maintainer = sandogasa_inventory::load(&keeps_files[0])?
        .inventory
        .maintainer;
    let mut quiet = !interactive;

    for _round in 0..5 {
        // Keeps per file, and the union.
        let mut keep_file_of: BTreeMap<String, String> = BTreeMap::new();
        for f in &keeps_files {
            for n in names_of(f)? {
                keep_file_of.entry(n).or_insert_with(|| f.clone());
            }
        }
        let keeps: BTreeSet<String> = keep_file_of.keys().cloned().collect();
        let roots: BTreeSet<String> = graph.roots.iter().cloned().collect();

        // 1. New keeps: walk them. A root nothing reaches any more was
        // a keep once; the derived recompute below drops its tail.
        let new_keeps: Vec<String> = keeps.difference(&roots).cloned().collect();
        let reachable = graph.reachable(&keeps);
        // Retired keeps are essential but deliberately not walked: not
        // dropped, just parked.
        let dropped: Vec<String> = roots
            .iter()
            .filter(|r| !keeps.contains(*r) && !reachable.contains(*r) && !retired.contains(*r))
            .cloned()
            .collect();
        for d in &dropped {
            if !report.dropped_keeps.contains(d) {
                report.dropped_keeps.push(d.clone());
            }
        }
        if !new_keeps.is_empty() && opts.dry_run {
            report.new_keeps.extend(new_keeps.iter().cloned());
        } else if !new_keeps.is_empty() {
            if opts.verbose {
                eprintln!(
                    "[reconcile] {}: walking {} new keep(s)",
                    c.name,
                    new_keeps.len()
                );
            }
            let (walk, offline, online) = walk_new(c, &mut graph, &new_keeps, owned, opts.verbose)?;
            report.capabilities_offline += offline;
            report.capabilities_online += online;
            write_graph(&graph_path, &graph)?;
            report.new_keeps.extend(new_keeps.iter().cloned());
            if let Some(closure_file) = c.closure.as_deref().map(resolve) {
                let pkgs: Vec<(String, String)> = walk
                    .collected
                    .iter()
                    .map(|d| {
                        let reason = if d.via.is_empty() {
                            format!("runtime dependency ({})", d.repoid)
                        } else {
                            format!(
                                "runtime dependency ({}): {} requires {}",
                                d.repoid, d.required_by, d.via
                            )
                        };
                        (d.source.clone(), reason)
                    })
                    .collect();
                report
                    .closure_added
                    .extend(add_packages(&closure_file, &keeps_files[0], &pkgs)?);
            }
        }

        // 2. Derived: recompute, then the decisions.
        let mut promoted_now: Vec<String> = Vec::new();
        if let Some(derived_path) = c.derived.as_deref().map(resolve) {
            let current = match std::path::Path::new(&derived_path).exists() {
                true => names_of(&derived_path)?,
                false => Default::default(),
            };
            let d = derive::derive(&graph, &keeps, owned, &current);
            let reasons: BTreeMap<&str, &Option<String>> = d
                .derived
                .iter()
                .map(|x| (x.name.as_str(), &x.reason))
                .collect();
            // A new derived package rides along unless the user wants it
            // as a keep in its own right (never for an external closure:
            // those keeps are not ours to add to).
            for name in &d.added {
                if report.derived_added.contains(name) {
                    continue;
                }
                let mut choice = 'r';
                if !quiet && !c.external {
                    let why = reasons
                        .get(name.as_str())
                        .and_then(|r| r.as_deref())
                        .unwrap_or("reachable from the keeps");
                    match ask(
                        &format!(
                            "{name} now rides in the derived inventory — {why}\n  (r)ide [default] / (e)ssential in its own right?"
                        ),
                        'r',
                        &['r', 'e'],
                    )? {
                        Answer::Choice(ch) => choice = ch,
                        Answer::AllDefault => quiet = true,
                        Answer::Quit => {
                            quiet = true;
                            interactive_off();
                        }
                    }
                }
                if choice == 'e' {
                    if !opts.dry_run {
                        kondo::file_into_inventory(&into, name, &maintainer)?;
                    }
                    promoted_now.push(name.clone());
                    report.promoted.push(name.clone());
                } else {
                    report.derived_added.push(name.clone());
                }
            }
            for name in &d.removed {
                if !report.derived_removed.contains(name) {
                    report.derived_removed.push(name.clone());
                }
            }
            // Promotions are keeps now: recompute without them before writing.
            let final_report = if promoted_now.is_empty() {
                d
            } else {
                let mut k = keeps.clone();
                k.extend(promoted_now.iter().cloned());
                derive::derive(&graph, &k, owned, &current)
            };
            if !opts.dry_run {
                crate::apply_derived(&derived_path, &keeps_files[0], &final_report)
                    .map_err(|e| e.to_string())?;
            }
        }

        // 3. Keeps only devel-only edges carry: offer to demote (ours only).
        if !c.external && c.derived.is_some() {
            let classified = dependents::classify(&graph, &keeps);
            let mut demote: Vec<String> = Vec::new();
            for carried in classified.carried.iter().filter(|k| k.devel_only) {
                if report.demoted.contains(&carried.name) || report.promoted.contains(&carried.name)
                {
                    continue;
                }
                if quiet {
                    break;
                }
                match ask(
                    &format!(
                        "{} is a keep only devel-only edges carry ({} → it)\n  (k)eep essential [default] / (d)emote to the derived inventory?",
                        carried.name,
                        carried.dependents.join(", ")
                    ),
                    'k',
                    &['k', 'd'],
                )? {
                    Answer::Choice('d') => demote.push(carried.name.clone()),
                    Answer::Choice(_) => {}
                    Answer::AllDefault | Answer::Quit => {
                        quiet = true;
                        break;
                    }
                }
            }
            if !demote.is_empty() {
                let mut by_file: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
                for n in &demote {
                    if let Some(f) = keep_file_of.get(n) {
                        by_file.entry(f.as_str()).or_default().insert(n.as_str());
                    }
                }
                if !opts.dry_run {
                    for (f, names) in by_file {
                        unkeep::remove_from_inventory(f, &names)?;
                    }
                }
                report.demoted.extend(demote);
                // The derived inventory changes with the keeps: one more round.
                continue;
            }
        }

        if promoted_now.is_empty() || opts.dry_run {
            break;
        }
    }
    Ok(report)
}

fn interactive_off() {
    eprintln!("(taking the defaults from here on)");
}

/// The human-readable summary of one closure.
fn format_closure(r: &ClosureReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let tag = if r.external { " (external)" } else { "" };
    let _ = writeln!(out, "closure {}{tag}:", r.name);
    if let Some(s) = &r.skipped {
        let _ = writeln!(out, "  skipped — {s}");
        return out;
    }
    let list = |label: &str, v: &[String]| -> String {
        if v.is_empty() {
            String::new()
        } else {
            let shown: Vec<&str> = v.iter().take(12).map(String::as_str).collect();
            let more = if v.len() > 12 {
                format!(", … {} more", v.len() - 12)
            } else {
                String::new()
            };
            format!("  {label} ({}): {}{more}\n", v.len(), shown.join(", "))
        }
    };
    out.push_str(&list("new keeps walked", &r.new_keeps));
    if !r.new_keeps.is_empty() {
        let _ = writeln!(
            out,
            "    {} capabilit{} answered from the graph, {} resolved live",
            r.capabilities_offline,
            if r.capabilities_offline == 1 {
                "y"
            } else {
                "ies"
            },
            r.capabilities_online
        );
    }
    out.push_str(&list("former keeps nothing reaches now", &r.dropped_keeps));
    out.push_str(&list("added to the closure inventory", &r.closure_added));
    out.push_str(&list("entered the derived inventory", &r.derived_added));
    out.push_str(&list("left the derived inventory", &r.derived_removed));
    out.push_str(&list("promoted to essential", &r.promoted));
    out.push_str(&list("demoted to derived", &r.demoted));
    if out.lines().count() == 1 {
        let _ = writeln!(out, "  nothing changed");
    }
    out
}

/// The `reconcile` subcommand.
pub fn run(opts: &Options) -> Result<Report, String> {
    let ws = opts.ws;
    let resolve = |p: &str| ws.dir.join(p).to_string_lossy().into_owned();
    let owned_path = ws
        .owned
        .as_deref()
        .map(resolve)
        .ok_or("the workspace has no `owned` inventory")?;
    let owned = names_of(&owned_path)?;
    let mut retired: BTreeSet<String> = BTreeSet::new();
    for p in &ws.retired {
        let path = resolve(p);
        if std::path::Path::new(&path).exists() {
            retired.extend(names_of(&path)?);
        }
    }
    let interactive = !opts.yes && !opts.json && std::io::stdin().is_terminal();

    let mut report = Report::default();
    for c in &ws.closures {
        let r = reconcile_closure(opts, c, &owned, &retired, interactive)?;
        if !opts.json {
            print!("{}", format_closure(&r));
        }
        report.closures.push(r);
    }

    // What nothing justifies, after every closure has had its say.
    let mut essential: BTreeSet<String> = BTreeSet::new();
    for p in ws.essential() {
        let path = resolve(&p);
        if std::path::Path::new(&path).exists() {
            essential.extend(names_of(&path)?);
        }
    }
    let prior = match ws.cull.as_deref().map(resolve) {
        Some(p) if std::path::Path::new(&p).exists() => kondo::prior_culled(&p)?,
        _ => Default::default(),
    };
    report.triage_candidates = owned
        .iter()
        .filter(|n| !essential.contains(*n) && !prior.contains(*n))
        .cloned()
        .collect();
    Ok(report)
}
