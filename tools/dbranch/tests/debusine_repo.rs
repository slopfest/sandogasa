// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `dbranch debusine-repo`, against a stub `curl` that answers the
//! existence checks from files and a stub `debusine` client that
//! records what it was asked to create. Nothing reaches the network:
//! each run's `PATH` holds only the stubs.

use std::path::Path;
use std::process::{Command, Stdio};

/// The stubs, reading and writing `state`: `wf` and `ws` hold the
/// HTTP status of the workflow template and the workspace pages, and
/// the client appends each call to `calls`, writing `created` once the
/// repository is created, after which the workspace answers 200.
fn stubs(state: &Path) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let s = state.display();
    let tools = [
        (
            "curl",
            format!(
                "for url; do :; done\n\
                 case \"$url\" in\n\
                 */workflow-template/*) read -r a < {s}/wf; echo \"$a\" ;;\n\
                 *) if [ -e {s}/created ]; then echo 200; else read -r a < {s}/ws; echo \"$a\"; fi ;;\n\
                 esac"
            ),
        ),
        (
            "debusine",
            format!(
                "echo \"$*\" >> {s}/calls\n\
                 case \"$*\" in *--data*) while read -r l; do echo \"  $l\" >> {s}/calls; done ;; esac\n\
                 case \"$*\" in *create-repository* | *--data*) : > {s}/created ;; esac"
            ),
        ),
    ];
    for (name, body) in tools {
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

/// Run `dbranch debusine-repo` for r-michel-dbranch with `extra`
/// arguments, with the pages answering `wf` and `ws`.
fn debusine_repo(wf: &str, ws: &str, extra: &[&str]) -> (bool, String, String) {
    let state = tempfile::tempdir().unwrap();
    std::fs::write(state.path().join("wf"), wf).unwrap();
    std::fs::write(state.path().join("ws"), ws).unwrap();
    let bin = stubs(state.path());
    let out = Command::new(env!("CARGO_BIN_EXE_dbranch"))
        .args([
            "--no-defaults",
            "debusine-repo",
            "--owner",
            "michel",
            "--project",
            "dbranch",
        ])
        .args(extra)
        .current_dir(state.path())
        .env("PATH", bin.path())
        .env("HOME", state.path())
        .env("XDG_CONFIG_HOME", state.path())
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let calls = std::fs::read_to_string(state.path().join("calls")).unwrap_or_default();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text, calls)
}

#[test]
fn an_existing_workflow_needs_nothing() {
    let (ok, out, calls) = debusine_repo("200", "200", &[]);
    assert!(ok, "{out}");
    assert!(calls.is_empty(), "{calls}");
}

#[test]
fn an_unclear_answer_is_a_note_not_a_failure() {
    let (ok, out, calls) = debusine_repo("503", "200", &[]);
    assert!(ok, "{out}");
    assert!(out.contains("could not tell whether the"), "{out}");
    assert!(calls.is_empty(), "{calls}");
    let (ok, out, _) = debusine_repo("404", "500", &[]);
    assert!(ok, "{out}");
    assert!(out.contains("the workspace r-michel-dbranch"), "{out}");
}

#[test]
fn without_a_terminal_or_yes_it_says_how_to_create_them() {
    let (ok, out, calls) = debusine_repo("404", "404", &[]);
    assert!(!ok);
    assert!(
        out.contains("does not exist (dput cannot create it)"),
        "{out}"
    );
    assert!(
        out.contains("create-repository") || out.contains("workflow start"),
        "{out}"
    );
    assert!(calls.is_empty(), "{calls}");

    let (ok, out, _) = debusine_repo("404", "200", &[]);
    assert!(!ok);
    assert!(out.contains("exists but has no"), "{out}");
}

#[test]
fn with_yes_it_creates_the_repository_and_then_the_suite() {
    let (ok, out, calls) = debusine_repo("404", "404", &["-y"]);
    assert!(ok, "{out}");
    // Each command, then (indented) what it read from stdin.
    let calls: Vec<&str> = calls.lines().collect();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(calls[0].contains("workflow start"), "{calls:?}");
    assert_eq!(calls[1], "  suffix: \"michel-dbranch\"");
    assert!(calls[2].starts_with("archive suite create"), "{calls:?}");
    assert!(calls[2].contains("r-michel-dbranch"), "{calls:?}");

    // A workspace that exists only needs its suite.
    let (ok, out, calls) = debusine_repo("404", "200", &["-y"]);
    assert!(ok, "{out}");
    assert_eq!(calls.lines().count(), 1, "{calls}");
}

#[test]
fn a_dry_run_shows_the_recipe_without_running_it() {
    let (ok, out, calls) = debusine_repo("404", "404", &["--dry-run"]);
    assert!(ok, "{out}");
    assert!(out.contains("if missing, offers to create them"), "{out}");
    assert!(calls.is_empty(), "{calls}");
}
