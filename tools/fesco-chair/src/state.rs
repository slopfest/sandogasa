// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Persisted agenda state, so decisions made while composing the
//! agenda (docs-item selections, section placement) aren't asked
//! again by `script` on meeting day. `agenda` and a fresh `script`
//! run save it; `script` reuses it when it matches the meeting date;
//! `summary` clears it once the meeting is over.

use std::path::{Path, PathBuf};

use chrono::NaiveDate;

use crate::sources::{Sections, Ticket};

/// The assembled agenda, as saved by `agenda`/`script`.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct AgendaState {
    pub date: NaiveDate,
    pub sections: Sections,
    /// Open fesco/docs items the chair chose *not* to put on the
    /// agenda (kept so a later run doesn't re-offer them).
    pub docs_open: Vec<Ticket>,
}

/// The state file path: `$XDG_STATE_HOME/fesco-chair/agenda.json`
/// (default `~/.local/state`).
pub fn state_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state")))?;
    Some(base.join("fesco-chair/agenda.json"))
}

/// Load the saved agenda for `date` from `path`. `None` when the file
/// is missing, unparseable, or for a different meeting date (stale).
pub fn load_from(path: &Path, date: NaiveDate) -> Option<AgendaState> {
    let text = std::fs::read_to_string(path).ok()?;
    let state: AgendaState = serde_json::from_str(&text).ok()?;
    (state.date == date).then_some(state)
}

/// Save the agenda state to `path`, creating parent directories.
pub fn save_to(path: &Path, state: &AgendaState) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Save to the default location, warning (not failing) on any problem
/// — the state is a convenience, never worth aborting a run over.
pub fn save(state: &AgendaState) {
    let Some(path) = state_path() else {
        return;
    };
    if let Err(e) = save_to(&path, state) {
        eprintln!("warning: could not save agenda state ({e})");
    }
}

/// Load from the default location.
pub fn load(date: NaiveDate) -> Option<AgendaState> {
    load_from(&state_path()?, date)
}

/// Remove the saved state, if any. Returns whether a file was
/// removed.
pub fn clear() -> bool {
    let Some(path) = state_path() else {
        return false;
    };
    std::fs::remove_file(&path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(date: NaiveDate) -> AgendaState {
        AgendaState {
            date,
            sections: Sections {
                voted: vec![],
                followups: vec![Ticket {
                    number: 3623,
                    title: "Planning for the Forgejo distgit migration".to_string(),
                    url: "https://forge.fedoraproject.org/fesco/tickets/issues/3623".to_string(),
                    decision: None,
                    repo: None,
                    pull: false,
                }],
                new_business: vec![],
            },
            docs_open: vec![Ticket {
                number: 28,
                title: "Clarify updates policy".to_string(),
                url: "https://forge.fedoraproject.org/fesco/docs/pulls/28".to_string(),
                decision: None,
                repo: Some("fesco/docs".to_string()),
                pull: true,
            }],
        }
    }

    #[test]
    fn state_round_trips_and_rejects_stale_dates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/agenda.json");
        let date = NaiveDate::from_ymd_opt(2026, 7, 7).unwrap();
        // Missing file → None.
        assert!(load_from(&path, date).is_none());
        save_to(&path, &state(date)).unwrap();
        let loaded = load_from(&path, date).unwrap();
        assert_eq!(loaded.sections.followups[0].number, 3623);
        assert_eq!(loaded.docs_open[0].label(), "fesco/docs#28");
        assert!(loaded.docs_open[0].pull);
        // A different meeting date is stale.
        let next = NaiveDate::from_ymd_opt(2026, 7, 14).unwrap();
        assert!(load_from(&path, next).is_none());
        // Garbage is ignored, not fatal.
        std::fs::write(&path, "not json").unwrap();
        assert!(load_from(&path, date).is_none());
    }
}
