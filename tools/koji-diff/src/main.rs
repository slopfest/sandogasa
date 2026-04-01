// SPDX-License-Identifier: MPL-2.0

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;

use koji_diff::diff::{self, PackageDiff};
use koji_diff::koji::{self, KojiClient, TaskInfo};
use koji_diff::parse::{self, KojiRef, RefType};
use koji_diff::xmlrpc;

#[derive(Parser)]
#[command(
    version,
    about,
    long_about = None,
    before_help = concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION"))
)]
struct Cli {
    /// First build reference (Koji URL, build:<ID>, or task:<ID>).
    ref1: String,

    /// Second build reference (Koji URL, build:<ID>, or task:<ID>).
    ref2: String,

    /// Koji instance hostname (required for bare IDs).
    #[arg(long, value_name = "HOST")]
    instance: Option<String>,

    /// Architecture to compare.
    #[arg(long, default_value = "x86_64")]
    arch: String,

    /// build.log tail lines on failure.
    #[arg(long = "log-lines", default_value = "50", value_name = "N")]
    build_log_lines: usize,

    /// Output as JSON.
    #[arg(long)]
    json: bool,

    /// Show debug info (log snippets, file listings).
    #[arg(long)]
    debug: bool,
}

#[derive(serde::Serialize)]
struct JsonOutput {
    instance: String,
    arch: String,
    ref1: JsonRef,
    ref2: JsonRef,
    package_diff: PackageDiff,
    #[serde(skip_serializing_if = "Option::is_none")]
    build_failure: Option<JsonBuildFailure>,
}

#[derive(serde::Serialize)]
struct JsonRef {
    input: String,
    ref_type: String,
    id: i64,
    build_arch_task_id: i64,
    state: String,
}

#[derive(serde::Serialize)]
struct JsonBuildFailure {
    task_id: i64,
    log_name: String,
    log_tail: String,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let ref1 = parse::parse_ref(&cli.ref1, cli.instance.as_deref())?;
    let ref2 = parse::parse_ref(&cli.ref2, cli.instance.as_deref())?;

    if ref1.instance != ref2.instance {
        return Err(format!(
            "both references must be from the same Koji instance\n\
             ref1: {}\n  ref2: {}",
            ref1.instance, ref2.instance
        )
        .into());
    }

    let koji_client = KojiClient::new(&ref1.instance);

    if !cli.json {
        eprintln!("koji-diff: comparing builds on {}", koji_client.instance());
    }

    // Resolve build IDs to task IDs.
    let task_id1 = resolve_task_id(&koji_client, &ref1)?;
    let task_id2 = resolve_task_id(&koji_client, &ref2)?;

    // Resolve to buildArch tasks.
    let arch_task1 = koji_client.resolve_build_arch_task(task_id1, &cli.arch)?;
    let arch_task2 = koji_client.resolve_build_arch_task(task_id2, &cli.arch)?;

    if !cli.json {
        eprintln!(
            "  ref1: {}",
            format_resolution(&ref1, task_id1, &arch_task1, &cli.arch)
        );
        eprintln!(
            "  ref2: {}",
            format_resolution(&ref2, task_id2, &arch_task2, &cli.arch)
        );
        eprintln!();
    }

    // Download logs from both tasks into temp directories.
    let tmpdir1 = tempfile::tempdir()?;
    let tmpdir2 = tempfile::tempdir()?;

    if !cli.json {
        eprintln!("Downloading logs from task {}...", arch_task1.id);
    }
    let root_log1 = koji_client.download_log(arch_task1.id, "root.log", tmpdir1.path())?;

    if !cli.json {
        eprintln!("Downloading logs from task {}...", arch_task2.id);
    }
    let root_log2 = koji_client.download_log(arch_task2.id, "root.log", tmpdir2.path())?;

    if cli.debug {
        debug_log_info("ref1", &root_log1, tmpdir1.path());
        debug_log_info("ref2", &root_log2, tmpdir2.path());
    }

    // Parse and diff packages.
    let pkgs1 = diff::parse_installed_packages(&root_log1);
    let pkgs2 = diff::parse_installed_packages(&root_log2);

    if !cli.json {
        eprintln!(
            "Parsed {} packages from ref1, {} from ref2\n",
            pkgs1.len(),
            pkgs2.len()
        );
    }

    let pkg_diff = diff::diff_packages(&pkgs1, &pkgs2);

    // Check for build failure and fetch build.log tail.
    // Reuse tmpdir if the failed task already had its logs downloaded,
    // otherwise create a new one.
    let build_failure = fetch_build_failure(
        &arch_task1,
        &arch_task2,
        tmpdir1.path(),
        tmpdir2.path(),
        cli.build_log_lines,
        cli.json,
    );

    if cli.json {
        let output = JsonOutput {
            instance: ref1.instance.clone(),
            arch: cli.arch.clone(),
            ref1: JsonRef {
                input: cli.ref1.clone(),
                ref_type: ref1.ref_type.to_string(),
                id: ref1.id,
                build_arch_task_id: arch_task1.id,
                state: arch_task1.state_name().to_string(),
            },
            ref2: JsonRef {
                input: cli.ref2.clone(),
                ref_type: ref2.ref_type.to_string(),
                id: ref2.id,
                build_arch_task_id: arch_task2.id,
                state: arch_task2.state_name().to_string(),
            },
            package_diff: pkg_diff,
            build_failure,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        let color = std::io::stdout().is_terminal();
        let label1 = format!("task {}", arch_task1.id);
        let label2 = format!("task {}", arch_task2.id);
        diff::print_diff(&pkg_diff, &label1, &label2, color);

        if let Some(failure) = &build_failure {
            println!(
                "\n--- Build failure in task {} ({} tail) ---",
                failure.task_id, failure.log_name
            );
            println!("{}", failure.log_tail);
        }
    }

    Ok(())
}

/// Try to read a file by name from a directory, searching recursively.
fn find_and_read(dir: &std::path::Path, name: &str) -> Option<String> {
    let direct = dir.join(name);
    std::fs::read_to_string(&direct).ok().or_else(|| {
        koji_diff::koji::find_file_in(dir, name).and_then(|p| std::fs::read_to_string(p).ok())
    })
}

fn debug_log_info(label: &str, log_content: &str, tmpdir: &std::path::Path) {
    eprintln!("--- DEBUG {label} ---");

    // List files in tmpdir.
    eprintln!("  files in tmpdir:");
    if let Ok(entries) = std::fs::read_dir(tmpdir) {
        for entry in entries.flatten() {
            let meta = entry.metadata().ok();
            let size = meta.map(|m| m.len()).unwrap_or(0);
            eprintln!("    {} ({size} bytes)", entry.file_name().to_string_lossy());
        }
    }

    // Show root.log stats and lines containing "Installed" or "nstall".
    let lines: Vec<_> = log_content.lines().collect();
    eprintln!(
        "  root.log: {} bytes, {} lines",
        log_content.len(),
        lines.len()
    );
    // Show a sample of parsed packages.
    let pkgs = diff::parse_installed_packages(log_content);
    eprintln!("  parsed {} packages (first 5):", pkgs.len());
    for pkg in pkgs.iter().take(5) {
        eprintln!("    {}", pkg.full);
    }
    eprintln!("---");
}

fn resolve_task_id(koji_client: &KojiClient, koji_ref: &KojiRef) -> Result<i64, xmlrpc::Error> {
    match koji_ref.ref_type {
        RefType::Task => Ok(koji_ref.id),
        RefType::Build => {
            let build = koji_client.get_build(koji_ref.id)?;
            Ok(build.task_id)
        }
    }
}

/// Format the resolution chain for display.
///
/// Shows how an input reference was resolved to a buildArch task:
/// - `build 2970832 (task 143889060) -> buildArch 143889177 (x86_64, CLOSED)`
/// - `task 143927217 -> buildArch 143927280 (x86_64, FAILED)`
/// - `task 143927280 (x86_64, FAILED)` (already a buildArch)
fn format_resolution(
    koji_ref: &KojiRef,
    parent_task_id: i64,
    arch_task: &TaskInfo,
    arch: &str,
) -> String {
    let arch_suffix = format!(
        "buildArch {} ({}, {})",
        arch_task.id,
        arch,
        arch_task.state_name()
    );

    // If the arch task IS the input, no chain needed.
    if koji_ref.ref_type == RefType::Task && koji_ref.id == arch_task.id {
        return format!(
            "task {} ({}, {})",
            arch_task.id,
            arch,
            arch_task.state_name()
        );
    }

    let input = match koji_ref.ref_type {
        RefType::Build => format!("build {} (task {})", koji_ref.id, parent_task_id),
        RefType::Task => {
            if parent_task_id == arch_task.id {
                // Task was already the buildArch task.
                return format!(
                    "task {} ({}, {})",
                    arch_task.id,
                    arch,
                    arch_task.state_name()
                );
            }
            format!("task {}", koji_ref.id)
        }
    };

    format!("{input} -> {arch_suffix}")
}

fn fetch_build_failure(
    task1: &TaskInfo,
    task2: &TaskInfo,
    tmpdir1: &std::path::Path,
    tmpdir2: &std::path::Path,
    lines: usize,
    quiet: bool,
) -> Option<JsonBuildFailure> {
    let (failed_task, tmpdir) = if task1.state == koji::TASK_FAILED {
        (task1, tmpdir1)
    } else if task2.state == koji::TASK_FAILED {
        (task2, tmpdir2)
    } else {
        return None;
    };

    // `koji download-logs` already downloaded all logs for this task.
    // Files may be in a subdirectory (koji uses arch-taskid dirs).
    // Prefer mock_output.log (shows dep resolution failures), fall back
    // to build.log (shows rpmbuild failures).
    let log_names = ["mock_output.log", "build.log"];
    for log_name in &log_names {
        let content = find_and_read(tmpdir, log_name);
        if let Some(text) = content {
            let all_lines: Vec<_> = text.lines().collect();
            let start = all_lines.len().saturating_sub(lines);
            let tail = all_lines[start..].join("\n");
            return Some(JsonBuildFailure {
                task_id: failed_task.id,
                log_name: log_name.to_string(),
                log_tail: tail,
            });
        }
    }

    if !quiet {
        eprintln!("(no failure logs found for task {})", failed_task.id);
    }
    None
}
