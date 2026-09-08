// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The workspace file (`kondo.toml`): what each inventory in a data
//! directory *is*, written once next to them, so every tool reading the
//! directory — poi-tracker's maintenance subcommands, pkg-health's
//! dependency walk — knows its inputs without a dozen flags per run.
//!
//! ```toml
//! # kondo.toml — poi-tracker workspace
//! user = "salimma"
//! owned = "inventory-salimma-direct.toml"
//! cull = "cull.toml"
//! retired = ["inventory-salimma-essential-retired.toml"]
//!
//! [[closure]]
//! name = "fedora"
//! keeps = ["inventory-salimma-essential.toml", "inventory-salimma-essential-rust.toml"]
//! closure = "fedora-build-deps.toml"
//! derived = "inventory-salimma-essential-deps.toml"
//! graph = "fedora-build-deps-graph.json"
//! branch = "rawhide"
//! from = ["rawhide"]
//!
//! [[closure]]
//! name = "hyperscale-el9"
//! external = true
//! keeps = ["inventory-hyperscale.toml"]
//! closure = "inventory-hyperscale-el9-deps.toml"
//! graph = "hyperscale-el9-graph.json"
//! branch = "hs.el9"
//! repo = "stack"
//! ```
//!
//! Paths are relative to the file. The file is turned into a
//! `[defaults]`-shaped table for the invoked subcommand (see
//! `sandogasa_cli::defaults`), so a flag on the command line still
//! wins, and `--no-defaults` ignores the file for a run.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The default file name, looked for in the current directory.
pub const DEFAULT_FILE: &str = "kondo.toml";

/// One dependency world: a set of keeps walked against one repo
/// configuration, with the graph and the inventories that walk writes.
#[derive(Debug, Clone, Deserialize)]
pub struct Closure {
    pub name: String,
    /// The inventories whose packages are kept — the roots of the walk.
    pub keeps: Vec<String>,
    /// The keeps are not ours on dist-git (another SIG's, another
    /// distribution's): we track their dependencies, never the
    /// packages themselves. Reports say so; nothing offers to cull or
    /// demote them. Parsed now so the file is complete; `reconcile`
    /// is what reads it.
    #[serde(default)]
    #[allow(dead_code)]
    pub external: bool,
    /// The walk's full collected-dependency inventory (`deps -o`).
    #[serde(default)]
    pub closure: Option<String>,
    /// The derived inventory: reachable ∩ owned ∖ keeps (`derive -o`).
    #[serde(default)]
    pub derived: Option<String>,
    /// The saved dependency graph (`deps --graph`).
    #[serde(default)]
    pub graph: Option<String>,
    /// fedrq branch (`rawhide`, `hs.el9`).
    pub branch: String,
    /// fedrq repo class (`stack`).
    #[serde(default)]
    pub repo: Option<String>,
    /// Repo ids providers are collected from (`--from`).
    #[serde(default)]
    pub from: Vec<String>,
    /// Base-distro repo id prefixes (`--base-repo`).
    #[serde(default)]
    pub base_repo: Vec<String>,
    /// Walk runtime dependencies only (`--runtime-only`).
    #[serde(default)]
    pub runtime_only: bool,
}

/// The workspace as written.
#[derive(Debug, Clone, Deserialize)]
pub struct Workspace {
    /// dist-git username whose access routes kondo and act.
    #[serde(default)]
    pub user: Option<String>,
    /// The packages you own directly (`sync-distgit --no-groups`).
    #[serde(default)]
    pub owned: Option<String>,
    /// The standing cull verdicts.
    #[serde(default)]
    pub cull: Option<String>,
    /// Essential inventories that are never walked (retired packages
    /// kept for older releases).
    #[serde(default)]
    pub retired: Vec<String>,
    #[serde(default, rename = "closure")]
    pub closures: Vec<Closure>,
    /// Where the file lives; paths resolve against it.
    #[serde(skip)]
    pub dir: PathBuf,
}

impl Workspace {
    /// Load `path`, or `./kondo.toml` when none is given and it exists.
    pub fn find(explicit: Option<&str>) -> Result<Option<(Self, PathBuf)>, String> {
        let path = match explicit {
            Some(p) => PathBuf::from(p),
            None => {
                let p = PathBuf::from(DEFAULT_FILE);
                if !p.exists() {
                    return Ok(None);
                }
                p
            }
        };
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let mut ws: Workspace =
            toml::from_str(&text).map_err(|e| format!("parsing {}: {e}", path.display()))?;
        ws.dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        if ws.closures.is_empty() {
            return Err(format!("{}: no [[closure]] entries", path.display()));
        }
        Ok(Some((ws, path)))
    }

    /// A path from the file, resolved against the directory it lives in.
    pub fn resolve(&self, p: &str) -> String {
        let joined = self.dir.join(p);
        joined.to_string_lossy().into_owned()
    }

    pub fn closure(&self, name: Option<&str>) -> Result<&Closure, String> {
        match name {
            None => Ok(&self.closures[0]),
            Some(n) => self.closures.iter().find(|c| c.name == n).ok_or_else(|| {
                format!(
                    "no closure named {n} in the workspace; have: {}",
                    self.closures
                        .iter()
                        .map(|c| c.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }),
        }
    }

    /// Everything kondo must treat as essential: every closure's keeps,
    /// its walk output and its derived inventory, and the retired ones.
    /// A closure output names packages that are not ours too, which is
    /// harmless — only owned packages can be cull candidates — and for
    /// an external closure that output *is* the essential list.
    pub fn essential(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in &self.closures {
            out.extend(c.keeps.iter().cloned());
            out.extend(c.closure.iter().cloned());
            out.extend(c.derived.iter().cloned());
        }
        out.extend(self.retired.iter().cloned());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Workspace {
        let mut ws: Workspace = toml::from_str(
            r#"
            user = "salimma"
            owned = "direct.toml"
            cull = "cull.toml"
            retired = ["retired.toml"]

            [[closure]]
            name = "fedora"
            keeps = ["essential.toml", "essential-rust.toml"]
            closure = "closure.toml"
            derived = "essential-deps.toml"
            graph = "graph.json"
            branch = "rawhide"
            from = ["rawhide"]

            [[closure]]
            name = "hs-el9"
            external = true
            keeps = ["hyperscale.toml"]
            derived = "hs-el9-deps.toml"
            branch = "hs.el9"
            repo = "stack"
            "#,
        )
        .unwrap();
        ws.dir = PathBuf::from("/data");
        ws
    }

    #[test]
    fn essential_is_every_keep_closure_derived_and_retired() {
        assert_eq!(
            sample().essential(),
            [
                "essential.toml",
                "essential-rust.toml",
                "closure.toml",
                "essential-deps.toml",
                "hyperscale.toml",
                "hs-el9-deps.toml",
                "retired.toml"
            ]
        );
    }
}
