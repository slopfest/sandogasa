// SPDX-License-Identifier: Apache-2.0 OR MIT
//! poi-tracker's view of the workspace file: the defaults each
//! subcommand takes from it. The file itself — what it holds and how it
//! is found — is [`sandogasa_inventory::workspace`], shared with every
//! tool that reads a data directory.

pub use sandogasa_inventory::workspace::{Closure, Workspace};

/// The CLI defaults a workspace supplies to each subcommand.
pub trait WorkspaceDefaults {
    /// The flag defaults for `subcommand`, from the workspace: the
    /// closure's walk flags, the inventories it names, the user. `None`
    /// for a subcommand the workspace has nothing to say to.
    fn defaults_for(
        &self,
        subcommand: &str,
        closure: Option<&str>,
    ) -> Result<Option<toml::Table>, String>;
}

impl WorkspaceDefaults for Workspace {
    fn defaults_for(
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
                set("from", Some(resolved_plain(&c.from)));
            }
            if !c.base_repo.is_empty() {
                set("base-repo", Some(resolved_plain(&c.base_repo)));
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
                        .map(|o| resolved(self, std::slice::from_ref(o))),
                );
                set("essential", Some(resolved(self, &self.essential())));
                set("user", user);
                set("output", s(&self.cull));
            }
            "act" => {
                set(
                    "inventory",
                    self.cull
                        .as_ref()
                        .map(|c| resolved(self, std::slice::from_ref(c))),
                );
                set("personal", s(&self.owned));
                set("user", user);
            }
            "announce" => {
                set(
                    "inventory",
                    self.cull
                        .as_ref()
                        .map(|c| resolved(self, std::slice::from_ref(c))),
                );
                set("user", user);
            }
            // The commands about the packages you maintain: the owned
            // inventory is their `-i`.
            "semver-audit" | "triage-updates" | "triage-retired" | "prune-retired" | "show"
            | "validate" => {
                set(
                    "inventory",
                    self.owned
                        .as_ref()
                        .map(|o| resolved(self, std::slice::from_ref(o))),
                );
            }
            "keep" => {
                let c = self.closure(closure)?;
                set("inventory", Some(resolved(self, &c.keeps)));
                set("graph", s(&c.graph));
                set("owned", s(&self.owned));
                set("deps", s(&c.derived));
                set("output", s(&c.closure));
                walk_flags(c, &mut set);
            }
            "deps" => {
                let c = self.closure(closure)?;
                set("inventory", Some(resolved(self, &c.keeps)));
                set("graph", s(&c.graph));
                set("output", s(&c.closure));
                set("fixpoint", s(&self.owned));
                walk_flags(c, &mut set);
            }
            "unkeep" => {
                let c = self.closure(closure)?;
                set("inventory", Some(resolved(self, &c.keeps)));
                set("graph", s(&c.graph));
                set(
                    "deps",
                    c.derived
                        .as_ref()
                        .map(|d| resolved(self, std::slice::from_ref(d))),
                );
            }
            "dependents" => {
                let c = self.closure(closure)?;
                set("inventory", Some(resolved(self, &c.keeps)));
                set("graph", s(&c.graph));
            }
            "derive" => {
                let c = self.closure(closure)?;
                set("inventory", Some(resolved(self, &c.keeps)));
                set("graph", s(&c.graph));
                set("owned", s(&self.owned));
                set("output", s(&c.derived));
            }
            _ => return Ok(None),
        }
        Ok(Some(t))
    }
}

/// Paths from the file, resolved, as a TOML array.
fn resolved(ws: &Workspace, paths: &[String]) -> toml::Value {
    toml::Value::Array(
        paths
            .iter()
            .map(|p| toml::Value::String(ws.resolve(p)))
            .collect(),
    )
}

/// Values that are not paths (repo ids), as a TOML array.
fn resolved_plain(items: &[String]) -> toml::Value {
    toml::Value::Array(items.iter().cloned().map(toml::Value::String).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
        assert_eq!(t["output"].as_str(), Some("/data/closure.toml"));
        assert_eq!(t["branch"].as_str(), Some("rawhide"));
        assert_eq!(t["from"].as_array().unwrap()[0].as_str(), Some("rawhide"));
        assert!(t.get("repo").is_none());
        let t = ws.defaults_for("keep", Some("hs-el9")).unwrap().unwrap();
        assert_eq!(t["branch"].as_str(), Some("hs.el9"));
        assert_eq!(t["repo"].as_str(), Some("stack"));
        assert!(t.get("graph").is_none(), "no graph configured for hs-el9");
        assert!(ws.defaults_for("keep", Some("nope")).is_err());
        // The owned inventory feeds the maintainer-side commands.
        let t = ws.defaults_for("semver-audit", None).unwrap().unwrap();
        assert_eq!(
            t["inventory"].as_array().unwrap()[0].as_str(),
            Some("/data/direct.toml")
        );
        assert!(ws.defaults_for("export", None).unwrap().is_none());
    }
}
