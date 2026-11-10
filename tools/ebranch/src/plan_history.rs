// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What changed in a rebuild plan since the last one sent (#85).
//!
//! Status updates on a rebuild campaign say what moved since the last
//! update: packages that appeared, packages built, pull requests opened
//! or merged. So each run's plan is kept, as `--json` writes it, under
//! `$XDG_STATE_HOME/ebranch/rebuild-plans/`, keyed by library, branch and
//! side tag, and the next report opens with the difference. Several runs
//! happen between updates, so the comparison is with the plan marked as
//! sent (`--mark-sent`, run after sending an update, which marks the
//! saved run without querying anything), or, before one is marked, with
//! the previous run. Two files per key are kept: the latest run and the
//! one sent.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A plan as kept between runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Saved {
    /// When the run finished.
    pub at: DateTime<Utc>,
    /// The plan, as `--json` prints it.
    pub plan: Value,
}

/// What changed since an earlier plan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Changes {
    /// When the plan compared with was made.
    pub since: String,
    /// Whether that plan was the one marked as sent, rather than the
    /// previous run.
    pub since_sent: bool,
    /// Packages to rebuild now that were not before, rebuilt or not.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<String>,
    /// Packages that were to rebuild and now are neither that nor done.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gone: Vec<String>,
    /// Packages rebuilt in the side tag since.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub built: Vec<String>,
    /// Pull requests opened since, `(package, url)`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub opened: Vec<(String, String)>,
    /// Pull requests merged since, `(package, url)`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub merged: Vec<(String, String)>,
}

/// Where a plan's runs are kept: one directory per library, branch and
/// side tag.
pub fn dir(library: &str, branch: &str, side_tag: Option<&str>) -> Result<PathBuf, String> {
    let base = dirs::state_dir()
        .ok_or("cannot tell where to keep the plan: neither $XDG_STATE_HOME nor $HOME is set")?;
    let safe = |s: &str| {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "._+-".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    let key = format!(
        "{}@{}@{}",
        safe(library),
        safe(branch),
        side_tag.map_or("none".to_string(), safe)
    );
    Ok(base.join("ebranch").join("rebuild-plans").join(key))
}

/// The plan kept as `name` (`latest` or `sent`) in `dir`, if any.
pub fn load(dir: &std::path::Path, name: &str) -> Option<Saved> {
    let text = std::fs::read_to_string(dir.join(format!("{name}.json"))).ok()?;
    serde_json::from_str(&text).ok()
}

/// Keep `saved` as `name` in `dir`.
pub fn store(dir: &std::path::Path, name: &str, saved: &Saved) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join(format!("{name}.json"));
    let text = serde_json::to_string_pretty(saved).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// What a plan says, for comparing: packages to rebuild (in rounds or
/// unbuildable), packages rebuilt, and pull requests by URL with their
/// package and state.
struct Facts {
    left: BTreeSet<String>,
    done: BTreeSet<String>,
    prs: BTreeMap<String, (String, String)>,
}

fn facts(plan: &Value) -> Facts {
    let names = |v: &Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect()
    };
    let mut left: BTreeSet<String> = plan["rounds"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(names)
        .collect();
    left.extend(
        plan["unbuildable"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|u| u["package"].as_str().map(str::to_string)),
    );
    let done = plan["dependents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|d| d.get("rebuilt").is_some_and(|r| !r.is_null()))
        .filter_map(|d| d["package"].as_str().map(str::to_string))
        .collect();
    let mut prs = BTreeMap::new();
    if let Some(map) = plan["pull_requests"].as_object() {
        for (pkg, list) in map {
            for pr in list.as_array().into_iter().flatten() {
                if let (Some(url), Some(state)) = (pr["url"].as_str(), pr["state"].as_str()) {
                    prs.insert(url.to_string(), (pkg.clone(), state.to_string()));
                }
            }
        }
    }
    Facts { left, done, prs }
}

/// What changed from `before` to `now`.
pub fn changes(before: &Saved, since_sent: bool, now: &Value) -> Changes {
    let (old, new) = (facts(&before.plan), facts(now));
    let added = new
        .left
        .iter()
        .filter(|p| !old.left.contains(*p) && !old.done.contains(*p))
        .cloned()
        .collect();
    let gone = old
        .left
        .iter()
        .filter(|p| !new.left.contains(*p) && !new.done.contains(*p))
        .cloned()
        .collect();
    let built = new.done.difference(&old.done).cloned().collect();
    let mut opened = Vec::new();
    let mut merged = Vec::new();
    for (url, (pkg, state)) in &new.prs {
        match (old.prs.get(url).map(|(_, s)| s.as_str()), state.as_str()) {
            (None, "open") => opened.push((pkg.clone(), url.clone())),
            (None | Some("open"), "merged") => merged.push((pkg.clone(), url.clone())),
            _ => {}
        }
    }
    Changes {
        since: before.at.format("%Y-%m-%d %H:%M UTC").to_string(),
        since_sent,
        added,
        gone,
        built,
        opened,
        merged,
    }
}

/// The report's opening section on `changes`, or on there being nothing
/// to compare with.
pub fn render(changes: Option<&Changes>) -> String {
    let Some(c) = changes else {
        return "## Since the last plan\n\nNo earlier plan to compare with: this run is \
                kept, and the next one says what changed since.\n\n"
            .to_string();
    };
    let what = if c.since_sent {
        "the plan sent"
    } else {
        "the previous run, as none is marked sent"
    };
    let mut out = format!("## Since {what} ({})\n\n", c.since);
    let list = |label: &str, items: &[String]| -> String {
        if items.is_empty() {
            String::new()
        } else {
            format!("- {label} ({}): {}\n", items.len(), items.join(", "))
        }
    };
    let prs = |label: &str, items: &[(String, String)]| -> String {
        if items.is_empty() {
            return String::new();
        }
        let links: Vec<String> = items
            .iter()
            .map(|(pkg, url)| format!("{pkg} ([PR]({url}))"))
            .collect();
        format!("- {label} ({}): {}\n", items.len(), links.join(", "))
    };
    let body = [
        list("built", &c.built),
        list("added to the plan", &c.added),
        list("no longer in the plan", &c.gone),
        prs("pull requests merged", &c.merged),
        prs("pull requests opened", &c.opened),
    ]
    .concat();
    if body.is_empty() {
        out.push_str("Nothing changed.\n\n");
    } else {
        out.push_str(&body);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn saved(at: &str, plan: Value) -> Saved {
        Saved {
            at: DateTime::parse_from_rfc3339(at)
                .unwrap()
                .with_timezone(&Utc),
            plan,
        }
    }

    /// The libical 4 campaign between two status updates: ical appeared,
    /// osmo was built, orage's PR merged and gnokii's opened.
    #[test]
    fn what_moved_since_the_plan_sent() {
        let before = saved(
            "2026-11-05T10:00:00Z",
            json!({
                "rounds": [["osmo", "orage"]],
                "unbuildable": [{"package": "noctalia"}],
                "dependents": [{"package": "osmo"}, {"package": "evolution", "rebuilt": "e-1"}],
                "pull_requests": {"orage": [{"url": "https://x/orage/pr/4", "state": "open"}]}
            }),
        );
        let now = json!({
            "rounds": [["orage", "ical", "gnokii"]],
            "unbuildable": [],
            "dependents": [
                {"package": "osmo", "rebuilt": "osmo-0.4.4-7"},
                {"package": "evolution", "rebuilt": "e-1"}
            ],
            "pull_requests": {
                "orage": [{"url": "https://x/orage/pr/4", "state": "merged"}],
                "gnokii": [{"url": "https://x/gnokii/pr/2", "state": "open"}]
            }
        });
        let c = changes(&before, true, &now);
        assert_eq!(c.built, ["osmo"]);
        assert_eq!(c.added, ["gnokii", "ical"]);
        assert_eq!(c.gone, ["noctalia"]);
        assert_eq!(c.merged, [("orage".into(), "https://x/orage/pr/4".into())]);
        assert_eq!(
            c.opened,
            [("gnokii".into(), "https://x/gnokii/pr/2".into())]
        );
        let text = render(Some(&c));
        assert!(
            text.starts_with("## Since the plan sent (2026-11-05 10:00 UTC)"),
            "{text}"
        );
        assert!(text.contains("- built (1): osmo\n"), "{text}");
    }

    #[test]
    fn nothing_to_compare_with_and_nothing_changed_are_said_plainly() {
        assert!(render(None).contains("No earlier plan to compare with"));
        let plan = json!({"rounds": [["osmo"]]});
        let c = changes(&saved("2026-11-08T09:00:00Z", plan.clone()), false, &plan);
        let text = render(Some(&c));
        assert!(
            text.contains("the previous run, as none is marked sent"),
            "{text}"
        );
        assert!(text.contains("Nothing changed."), "{text}");
    }

    #[test]
    fn plans_are_kept_per_library_branch_and_side_tag() {
        let d = dir("libical", "rawhide", Some("f45-build-side-123")).unwrap();
        assert!(d.ends_with("ebranch/rebuild-plans/libical@rawhide@f45-build-side-123"));
        let none = dir("libical", "rawhide", None).unwrap();
        assert!(none.ends_with("libical@rawhide@none"));
        let tmp = tempfile::tempdir().unwrap();
        let s = saved("2026-11-09T14:02:00Z", json!({"rounds": []}));
        store(tmp.path(), "latest", &s).unwrap();
        assert_eq!(load(tmp.path(), "latest").unwrap().at, s.at);
        assert!(load(tmp.path(), "sent").is_none());
    }
}
