// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The workspace file: what each inventory in a data directory *is*,
//! written once next to them, so the maintenance subcommands know
//! their inputs without a dozen flags per run.
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

    fn resolve(&self, p: &str) -> String {
        let joined = self.dir.join(p);
        joined.to_string_lossy().into_owned()
    }

    fn resolved(&self, paths: &[String]) -> toml::Value {
        toml::Value::Array(
            paths
                .iter()
                .map(|p| toml::Value::String(self.resolve(p)))
                .collect(),
        )
    }

    /// The closure named, or the first.
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

    /// The `[defaults]`-shaped table for `subcommand`: only the flags
    /// that subcommand has, so nothing is injected blind. `None` for
    /// a subcommand the workspace has nothing to say about.
    pub fn defaults_for(
        &self,
        subcommand: &str,
        closure: Option<&str>,
    ) -> Result<Option<toml::Table>, String> {
        let mut t = toml::Table::new();
        let mut set = |k: &str, v: Option<toml::Value>| {
            if let Some(v) = v {
                t.insert(k.to_string(), v);
            }
        };
        let s = |o: &Option<String>| o.as_ref().map(|p| toml::Value::String(self.resolve(p)));
        let user = self.user.clone().map(toml::Value::String);
        let walk_flags = |c: &Closure, set: &mut dyn FnMut(&str, Option<toml::Value>)| {
            set("branch", Some(toml::Value::String(c.branch.clone())));
            set("repo", c.repo.clone().map(toml::Value::String));
            if !c.from.is_empty() {
                set("from", Some(self.resolved_plain(&c.from)));
            }
            if !c.base_repo.is_empty() {
                set("base-repo", Some(self.resolved_plain(&c.base_repo)));
            }
            if c.runtime_only {
                set("runtime-only", Some(toml::Value::Boolean(true)));
            }
        };
        match subcommand {
            "kondo" => {
                set(
                    "inventory",
                    self.owned
                        .as_ref()
                        .map(|o| self.resolved(std::slice::from_ref(o))),
                );
                set("essential", Some(self.resolved(&self.essential())));
                set("user", user);
                set("output", s(&self.cull));
            }
            "act" => {
                set(
                    "inventory",
                    self.cull
                        .as_ref()
                        .map(|c| self.resolved(std::slice::from_ref(c))),
                );
                set("personal", s(&self.owned));
                set("user", user);
            }
            "announce" => {
                set(
                    "inventory",
                    self.cull
                        .as_ref()
                        .map(|c| self.resolved(std::slice::from_ref(c))),
                );
                set("user", user);
            }
            "keep" => {
                let c = self.closure(closure)?;
                set("inventory", Some(self.resolved(&c.keeps)));
                set("graph", s(&c.graph));
                set("owned", s(&self.owned));
                set("deps", s(&c.derived));
                walk_flags(c, &mut set);
            }
            "deps" => {
                let c = self.closure(closure)?;
                set("inventory", Some(self.resolved(&c.keeps)));
                set("graph", s(&c.graph));
                set("output", s(&c.closure));
                set("fixpoint", s(&self.owned));
                walk_flags(c, &mut set);
            }
            "unkeep" => {
                let c = self.closure(closure)?;
                set("inventory", Some(self.resolved(&c.keeps)));
                set("graph", s(&c.graph));
                set(
                    "deps",
                    c.derived
                        .as_ref()
                        .map(|d| self.resolved(std::slice::from_ref(d))),
                );
            }
            "dependents" => {
                let c = self.closure(closure)?;
                set("inventory", Some(self.resolved(&c.keeps)));
                set("graph", s(&c.graph));
            }
            "derive" => {
                let c = self.closure(closure)?;
                set("inventory", Some(self.resolved(&c.keeps)));
                set("graph", s(&c.graph));
                set("owned", s(&self.owned));
                set("output", s(&c.derived));
            }
            _ => return Ok(None),
        }
        Ok(Some(t))
    }

    /// Values that are not paths (repo ids), as a TOML array.
    fn resolved_plain(&self, items: &[String]) -> toml::Value {
        toml::Value::Array(items.iter().cloned().map(toml::Value::String).collect())
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

    #[test]
    fn kondo_defaults_name_owned_essential_user_and_cull() {
        let t = sample().defaults_for("kondo", None).unwrap().unwrap();
        assert_eq!(
            t["inventory"].as_array().unwrap()[0].as_str(),
            Some("/data/direct.toml")
        );
        assert_eq!(t["essential"].as_array().unwrap().len(), 7);
        assert_eq!(t["user"].as_str(), Some("salimma"));
        assert_eq!(t["output"].as_str(), Some("/data/cull.toml"));
    }

    #[test]
    fn keep_defaults_follow_the_chosen_closure() {
        let ws = sample();
        let t = ws.defaults_for("keep", None).unwrap().unwrap();
        assert_eq!(t["graph"].as_str(), Some("/data/graph.json"));
        assert_eq!(t["branch"].as_str(), Some("rawhide"));
        assert_eq!(t["from"].as_array().unwrap()[0].as_str(), Some("rawhide"));
        assert!(t.get("repo").is_none());
        let t = ws.defaults_for("keep", Some("hs-el9")).unwrap().unwrap();
        assert_eq!(t["branch"].as_str(), Some("hs.el9"));
        assert_eq!(t["repo"].as_str(), Some("stack"));
        assert!(t.get("graph").is_none(), "no graph configured for hs-el9");
        assert!(ws.defaults_for("keep", Some("nope")).is_err());
        assert!(ws.defaults_for("show", None).unwrap().is_none());
    }
}
