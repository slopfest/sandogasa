// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A karma-style review of the builds in a SIG's `-testing` tags (#144),
//! modeled on fedora-easy-karma: each build is shown, and the verdict
//! acts on it at once.
//!
//! - `+1` releases it: tagged into the sibling `-release` tag and
//!   untagged from `-testing` ([`crate::packages::promote_changes`]),
//!   and its tracking issue is told, as `cbspkg promote` tells it.
//! - `-1` rejects it: untagged from `-testing`, and its tracking issue is
//!   told why and labeled `stage::rejected`.
//! - `0` or Enter leaves it; `q` stops.
//!
//! The verdict is the confirmation: the comment is shown before it is
//! posted, with no second question. What builds to list and what to show
//! about each are the SIG tracker's (`hs-relmon review`,
//! `cpu-sig-tracker review`); the loop, the tag changes and the tracking
//! issue are shared here.

use std::io::{BufRead, Write};

use crate::notify::{self, Branch, Checkout, Event, MergeRequest};
use crate::tags::{TagChange, TagHub};
use crate::tracking::{Provenance, UntagReason};
use crate::{Collection, Sig, Stage};

/// One build to review, the `-testing` tag it is in, and the `-release`
/// tag a promotion moves it to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewItem {
    pub testing_tag: String,
    pub release_tag: String,
    pub nvr: String,
}

/// A reviewer's verdict on one build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Karma {
    /// Tag into `-release` and untag from `-testing`.
    Promote,
    /// Untag from `-testing`.
    Reject,
    /// Leave the build where it is.
    Skip,
    /// Stop reviewing.
    Quit,
}

/// Where a reviewed build's tracking issue is, for telling it: the SIG,
/// the package, and the SIG branch the build came from
/// (`c10s-proposed-RHEL-306261`, which names a Proposed Update's issue;
/// a Hyperscale issue is found by its package, so for Hyperscale this
/// only names the build's line in messages).
///
/// A review does not close a Hyperscale issue once every release has its
/// version, as `cbspkg promote` does: without the package's checkout it
/// cannot tell which releases the SIG carries the package for, and a
/// release not built yet would be missed. hs-relmon's scan closes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemContext {
    pub sig: Sig,
    pub package: String,
    pub branch: String,
}

/// GitLab, for telling the tracking issues; `token: None` shows what would
/// be posted, with nothing posted.
pub struct Tracking<'a> {
    pub base: &'a str,
    pub token: Option<&'a str>,
    /// The reviewing tool, whose `config` sets the token.
    pub tool: &'static str,
}

/// What a review did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    pub promoted: usize,
    pub rejected: usize,
    pub skipped: usize,
}

/// A checkout for a review, which has none: what a build carries is not
/// told, and no release tags, so no issue is closed ([`ItemContext`]).
struct NoCheckout;

/// Parse a karma answer: `+1`/`1`, `-1`, `0`/`s`/empty (skip), `q`
/// (quit). `None` for anything else, to ask again.
pub fn parse_karma(input: &str) -> Option<Karma> {
    match input.trim() {
        "+1" | "1" => Some(Karma::Promote),
        "-1" => Some(Karma::Reject),
        "0" | "s" | "S" | "" => Some(Karma::Skip),
        "q" | "Q" => Some(Karma::Quit),
        _ => None,
    }
}

/// Ask for a verdict at the terminal until one is given; `Ok(None)` at
/// the end of input, which ends the review.
pub fn prompt_karma() -> Result<Option<Karma>, String> {
    loop {
        eprint!("karma [+1=promote / -1=reject / Enter=skip / q=quit]: ");
        std::io::stderr().flush().map_err(|e| e.to_string())?;
        let mut line = String::new();
        let n = std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(None);
        }
        match parse_karma(&line) {
            Some(k) => return Ok(Some(k)),
            None => eprintln!("unrecognized input: {:?}", line.trim()),
        }
    }
}

/// The tag changes a verdict makes on `item`: a promotion's
/// ([`crate::packages::promote_changes`], from `-testing`), or the
/// untag from `-testing` a rejection is; none otherwise.
pub fn verdict_changes(item: &ReviewItem, karma: Karma) -> Result<Vec<TagChange>, String> {
    match karma {
        Karma::Promote => {
            let (collection, _) = Collection::from_tag(&item.testing_tag)
                .ok_or_else(|| format!("{} is not a SIG's -testing tag", item.testing_tag))?;
            Ok(crate::packages::promote_changes(
                &collection,
                &item.nvr,
                Stage::Release,
                std::slice::from_ref(&item.testing_tag),
            ))
        }
        Karma::Reject => Ok(vec![TagChange::untag(&item.testing_tag, &item.nvr)]),
        Karma::Skip | Karma::Quit => Ok(Vec::new()),
    }
}

/// Review `items` in turn: `show` prints each (with its place and the
/// total), `karma` gives the verdict (`None` ends the review, as the end
/// of input does), `hub` makes the tag changes, and, with `tracking`,
/// each build's tracking issue (found from `context`) is told of a
/// verdict that changed something. A failed tag change ends the review
/// with its error, after telling the issue of what was done.
pub fn review(
    items: &[ReviewItem],
    show: &mut dyn FnMut(&ReviewItem, usize, usize),
    karma: &mut dyn FnMut() -> Result<Option<Karma>, String>,
    hub: &dyn TagHub,
    tracking: Option<&Tracking>,
    context: &dyn Fn(&ReviewItem) -> Option<ItemContext>,
) -> Result<Tally, String> {
    let mut tally = Tally::default();
    let total = items.len();
    for (i, item) in items.iter().enumerate() {
        show(item, i + 1, total);
        let Some(verdict) = karma()? else {
            eprintln!("\n(exiting)");
            break;
        };
        match verdict {
            Karma::Quit => break,
            Karma::Skip => {
                tally.skipped += 1;
                continue;
            }
            Karma::Promote | Karma::Reject => {}
        }
        let changes = verdict_changes(item, verdict)?;
        // Where the issue is, worked out only when it is to be told.
        let told = tracking.and_then(|t| context(item).map(|c| (t, c)));
        let notices = match &told {
            Some((t, c)) => plan(t, c, item, verdict, &changes),
            None => Vec::new(),
        };
        for n in &notices {
            eprint!(
                "{}",
                notify::describe(n, told.as_ref().is_some_and(|(t, _)| t.token.is_some()))
            );
        }
        let mut done = Vec::new();
        let made = apply(hub, &changes, verdict, &mut done);
        if let Some((t, _)) = &told {
            notify::post(
                t.base,
                t.token,
                &notices,
                &done,
                &crate::packages::latest_in_tags,
            );
        }
        // A promotion that failed partway, tagged into -release but still
        // in -testing, is finished by `cbspkg promote`, which completes
        // the untag and tells the issue.
        made.map_err(|e| match verdict {
            Karma::Promote => format!(
                "{e}; to finish releasing {}, run `cbspkg promote {}` in its checkout",
                item.nvr, item.nvr
            ),
            _ => e,
        })?;
        match verdict {
            Karma::Promote => tally.promoted += 1,
            _ => tally.rejected += 1,
        }
    }
    Ok(tally)
}

impl Checkout for NoCheckout {
    fn fetch_missing(&self, _: &MergeRequest, _: &[Option<String>]) {}

    fn provenance(&self, _: &str, _: &str) -> Provenance {
        Provenance::Unknown
    }

    fn release_tags(&self) -> Vec<String> {
        Vec::new()
    }
}

/// What to tell `item`'s tracking issue of `verdict`.
fn plan(
    t: &Tracking,
    c: &ItemContext,
    item: &ReviewItem,
    verdict: Karma,
    changes: &[TagChange],
) -> Vec<notify::Notice> {
    let on = Branch {
        sig: c.sig,
        package: &c.package,
        branch: &c.branch,
        checkout: &NoCheckout,
        dry_run: false,
        tool: t.tool,
    };
    match verdict {
        Karma::Promote => notify::plan(
            t.base,
            t.token,
            &on,
            &[(item.nvr.clone(), Stage::Release, changes.to_vec())],
            // A review has no checkout to say what a build carries.
            &|nvrs: &[String]| Ok(vec![None; nvrs.len()]),
        ),
        Karma::Reject => notify::plan_untags(
            t.base,
            t.token,
            &on,
            &[(item.nvr.clone(), item.testing_tag.clone())],
            &UntagReason::Rejected,
        ),
        Karma::Skip | Karma::Quit => Vec::new(),
    }
}

/// Make `changes` for `verdict`, recording in `done` what the tracking
/// issue can be told: the release once every change is made, each tag
/// left as it is.
fn apply(
    hub: &dyn TagHub,
    changes: &[TagChange],
    verdict: Karma,
    done: &mut Vec<(String, Event)>,
) -> Result<(), String> {
    for change in changes {
        let why = match change {
            TagChange::Tag { tag, nvr } => format!("Tag {nvr} into {tag}"),
            TagChange::Untag { tag, nvr } => format!("Untag {nvr} from {tag}"),
        };
        hub.apply(change, &why)
            .map_err(|e| format!("{}: {e}", change.argv().join(" ")))?;
        eprintln!(
            "{} {} {} {}",
            match change {
                TagChange::Tag { .. } => "tagged",
                TagChange::Untag { .. } => "untagged",
            },
            change.nvr(),
            match change {
                TagChange::Tag { .. } => "into",
                TagChange::Untag { .. } => "from",
            },
            change.tag_name()
        );
        if verdict == Karma::Reject {
            done.push((
                change.nvr().to_string(),
                Event::Untagged(change.tag_name().to_string()),
            ));
        }
    }
    if verdict == Karma::Promote
        && let Some(c) = changes.first()
    {
        done.push((c.nvr().to_string(), Event::Reached(Stage::Release)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;

    const TESTING: &str = "hyperscale10s-packages-main-testing";
    const RELEASE: &str = "hyperscale10s-packages-main-release";
    const NVR: &str = "ethtool-7.1-1.hs.el10";

    fn item() -> ReviewItem {
        ReviewItem {
            testing_tag: TESTING.into(),
            release_tag: RELEASE.into(),
            nvr: NVR.into(),
        }
    }

    /// A hub recording the changes asked of it, failing those whose tag
    /// is in `refuse`.
    #[derive(Default)]
    struct FakeHub {
        made: RefCell<Vec<String>>,
        refuse: Vec<&'static str>,
    }

    impl TagHub for FakeHub {
        fn apply(&self, change: &TagChange, _why: &str) -> Result<(), Box<dyn std::error::Error>> {
            if self.refuse.contains(&change.tag_name()) {
                return Err("refused".into());
            }
            self.made.borrow_mut().push(change.argv().join(" "));
            Ok(())
        }
    }

    /// Answer `verdicts` in turn, then the end of input.
    fn answers(verdicts: &[Karma]) -> impl FnMut() -> Result<Option<Karma>, String> {
        let mut left: Vec<Karma> = verdicts.iter().rev().copied().collect();
        move || Ok(left.pop())
    }

    #[test]
    fn parse_karma_accepts_all_forms() {
        assert_eq!(parse_karma("+1"), Some(Karma::Promote));
        assert_eq!(parse_karma(" 1 "), Some(Karma::Promote));
        assert_eq!(parse_karma("-1"), Some(Karma::Reject));
        assert_eq!(parse_karma("0"), Some(Karma::Skip));
        assert_eq!(parse_karma(""), Some(Karma::Skip));
        assert_eq!(parse_karma("Q"), Some(Karma::Quit));
        assert_eq!(parse_karma("maybe"), None);
    }

    #[test]
    fn a_verdict_makes_its_tag_changes() {
        let lines = |k| {
            verdict_changes(&item(), k)
                .unwrap()
                .iter()
                .map(|c| c.argv().join(" "))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            lines(Karma::Promote),
            [
                format!("cbs tag-build {RELEASE} {NVR}"),
                format!("cbs untag-build {TESTING} {NVR}")
            ]
        );
        assert_eq!(
            lines(Karma::Reject),
            [format!("cbs untag-build {TESTING} {NVR}")]
        );
        assert!(lines(Karma::Skip).is_empty());
        let odd = ReviewItem {
            testing_tag: "no-such-thing".into(),
            ..item()
        };
        assert!(verdict_changes(&odd, Karma::Promote).is_err());
    }

    #[test]
    fn each_verdict_acts_and_is_counted() {
        let items = [item(), item(), item(), item()];
        let hub = FakeHub::default();
        let mut shown = Vec::new();
        let tally = review(
            &items,
            &mut |it, i, total| shown.push(format!("{i}/{total} {}", it.nvr)),
            &mut answers(&[Karma::Promote, Karma::Skip, Karma::Reject, Karma::Quit]),
            &hub,
            None,
            &|_| None,
        )
        .unwrap();
        assert_eq!(
            tally,
            Tally {
                promoted: 1,
                rejected: 1,
                skipped: 1
            }
        );
        assert_eq!(shown.len(), 4);
        assert_eq!(hub.made.borrow().len(), 3);
        // The end of input ends the review too.
        let tally = review(
            &items,
            &mut |_, _, _| {},
            &mut answers(&[Karma::Skip]),
            &hub,
            None,
            &|_| None,
        )
        .unwrap();
        assert_eq!(tally.skipped, 1);
    }

    #[test]
    fn a_refused_change_ends_the_review_with_its_error() {
        let hub = FakeHub {
            refuse: vec![TESTING],
            ..FakeHub::default()
        };
        let err = review(
            &[item(), item()],
            &mut |_, _, _| {},
            &mut answers(&[Karma::Promote, Karma::Promote]),
            &hub,
            None,
            &|_| None,
        )
        .unwrap_err();
        assert!(err.starts_with("cbs untag-build"), "{err}");
        assert!(
            err.ends_with(&format!("run `cbspkg promote {NVR}` in its checkout")),
            "{err}"
        );
        // The tag into -release was made before the untag failed.
        assert_eq!(hub.made.borrow().len(), 1);
    }

    #[test]
    fn the_issue_can_be_told_what_was_done() {
        let hub = FakeHub::default();
        let mut done = Vec::new();
        let promote = verdict_changes(&item(), Karma::Promote).unwrap();
        apply(&hub, &promote, Karma::Promote, &mut done).unwrap();
        assert_eq!(done, [(NVR.to_string(), Event::Reached(Stage::Release))]);
        let mut done = Vec::new();
        let reject = verdict_changes(&item(), Karma::Reject).unwrap();
        apply(&hub, &reject, Karma::Reject, &mut done).unwrap();
        assert_eq!(done, [(NVR.to_string(), Event::Untagged(TESTING.into()))]);
        // A promotion that failed partway is not told as a release.
        let refusing = FakeHub {
            refuse: vec![TESTING],
            ..FakeHub::default()
        };
        let mut done = Vec::new();
        assert!(apply(&refusing, &promote, Karma::Promote, &mut done).is_err());
        assert!(done.is_empty());
    }

    /// A rejected Hyperscale build: its new-version issue is told, and
    /// labeled stage::rejected.
    #[test]
    fn a_rejection_is_told_to_the_tracking_issue() {
        let mut server = mockito::Server::new();
        let path = "/api/v4/projects/CentOS%2FHyperscale%2Frpms%2Fethtool";
        server
            .mock("GET", format!("{path}/issues").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(
                serde_json::json!([{"iid": 2, "title": "ethtool-7.1 is available",
                    "state": "opened", "web_url": "h2", "labels": ["rfe::new-version"]}])
                .to_string(),
            )
            .create();
        server
            .mock("GET", format!("{path}/issues/2/notes").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body("[]")
            .create();
        server
            .mock("GET", format!("{path}/labels").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"[{"name": "stage::rejected"}]"#)
            .create();
        let comment = server
            .mock("POST", format!("{path}/issues/2/notes").as_str())
            .match_body(mockito::Matcher::Regex("rejected in review".into()))
            .with_status(201)
            .with_body(r#"{"id": 1, "body": "b"}"#)
            .expect(1)
            .create();
        let label = server
            .mock("PUT", format!("{path}/issues/2").as_str())
            .match_body(mockito::Matcher::PartialJson(
                serde_json::json!({"add_labels": "stage::rejected"}),
            ))
            .with_status(200)
            .with_body(r#"{"iid": 2, "title": "t", "state": "opened", "web_url": "h2"}"#)
            .expect(1)
            .create();
        let url = server.url();
        let tracking = Tracking {
            base: &url,
            token: Some("t"),
            tool: "hs-relmon",
        };
        let context = |it: &ReviewItem| {
            Some(ItemContext {
                sig: Sig::Hyperscale,
                package: "ethtool".into(),
                branch: it.testing_tag.clone(),
            })
        };
        let tally = review(
            &[item()],
            &mut |_, _, _| {},
            &mut answers(&[Karma::Reject]),
            &FakeHub::default(),
            Some(&tracking),
            &context,
        )
        .unwrap();
        assert_eq!(tally.rejected, 1);
        comment.assert();
        label.assert();
    }
}
