// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Changes to a build's tags in CBS: tagging a build into a tag, or
//! untagging it from one (#144). A tool moving SIG builds describes the
//! move as a [`TagChange`] and makes it through a [`TagHub`], so the
//! command a person could type is the same in each, and a test can
//! record the moves instead of running `cbs`. `cbspkg promote` does so;
//! the trackers' untag and review commands are moving to it.

use std::path::Path;

/// One change to one build's tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagChange {
    /// Tag `nvr` into `tag`.
    Tag { tag: String, nvr: String },
    /// Untag `nvr` from `tag`.
    Untag { tag: String, nvr: String },
}

/// Where tag changes are made: CBS through its `cbs` command
/// ([`CbsCli`]), or a fake in tests.
pub trait TagHub {
    /// Make `change`, with `why` saying what it is for, as a step
    /// heading where steps are narrated.
    fn apply(&self, change: &TagChange, why: &str) -> Result<(), Box<dyn std::error::Error>>;
}

/// CBS, through the `cbs` command, run as `ui` runs commands: shown
/// under `--explain` (and confirmed step by step), only shown under
/// `--dry-run`. `cwd` is where the command runs.
pub struct CbsCli<'a> {
    pub ui: &'a sandogasa_cli::explain::Ui,
    pub cwd: &'a Path,
}

impl TagChange {
    /// Tag `nvr` into `tag`.
    pub fn tag(tag: &str, nvr: &str) -> Self {
        TagChange::Tag {
            tag: tag.to_string(),
            nvr: nvr.to_string(),
        }
    }

    /// Untag `nvr` from `tag`.
    pub fn untag(tag: &str, nvr: &str) -> Self {
        TagChange::Untag {
            tag: tag.to_string(),
            nvr: nvr.to_string(),
        }
    }

    /// The tag the change is to or from.
    pub fn tag_name(&self) -> &str {
        match self {
            TagChange::Tag { tag, .. } | TagChange::Untag { tag, .. } => tag,
        }
    }

    /// The build the change moves.
    pub fn nvr(&self) -> &str {
        match self {
            TagChange::Tag { nvr, .. } | TagChange::Untag { nvr, .. } => nvr,
        }
    }

    /// The `cbs` command that makes the change, as a person would type
    /// it: `cbs tag-build <tag> <nvr>`.
    pub fn argv(&self) -> Vec<String> {
        let verb = match self {
            TagChange::Tag { .. } => "tag-build",
            TagChange::Untag { .. } => "untag-build",
        };
        ["cbs", verb, self.tag_name(), self.nvr()]
            .map(String::from)
            .to_vec()
    }
}

/// The first change among `changes` that tags a build, and the first
/// that untags one: where a promotion takes a build, and where from.
pub fn tag_and_untag(changes: &[TagChange]) -> (Option<&str>, Option<&str>) {
    let tag = changes.iter().find_map(|c| match c {
        TagChange::Tag { tag, .. } => Some(tag.as_str()),
        TagChange::Untag { .. } => None,
    });
    let untag = changes.iter().find_map(|c| match c {
        TagChange::Untag { tag, .. } => Some(tag.as_str()),
        TagChange::Tag { .. } => None,
    });
    (tag, untag)
}

impl TagHub for CbsCli<'_> {
    fn apply(&self, change: &TagChange, why: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.ui.step(why);
        self.ui.run_required(&change.argv(), self.cwd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_is_the_command_a_person_would_type() {
        let tag = TagChange::tag("hyperscale10s-packages-main-release", "foo-1-1.hs.el10");
        assert_eq!(
            tag.argv(),
            [
                "cbs",
                "tag-build",
                "hyperscale10s-packages-main-release",
                "foo-1-1.hs.el10"
            ]
        );
        let untag = TagChange::untag("hyperscale10s-packages-main-testing", "foo-1-1.hs.el10");
        assert_eq!(untag.argv()[1], "untag-build");
        assert_eq!(untag.tag_name(), "hyperscale10s-packages-main-testing");
        assert_eq!(untag.nvr(), "foo-1-1.hs.el10");
        assert_eq!(
            tag_and_untag(&[untag.clone(), tag.clone()]),
            (
                Some("hyperscale10s-packages-main-release"),
                Some("hyperscale10s-packages-main-testing")
            )
        );
        assert_eq!(tag_and_untag(&[]), (None, None));
    }

    #[test]
    fn a_dry_run_only_shows_the_change() {
        let ui = sandogasa_cli::explain::Ui {
            explain: false,
            dry_run: true,
            quiet: true,
        };
        let hub = CbsCli {
            ui: &ui,
            cwd: Path::new("."),
        };
        // `cbs` need not exist: a dry run does not run it.
        hub.apply(&TagChange::tag("t", "n-1-1"), "Tag n-1-1 into t")
            .unwrap();
    }
}
