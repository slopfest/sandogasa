// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `script` subcommand — the day-of zodbot script for whoever chairs
//! the meeting: the SIG's meeting checklist as `!topic` lines, with
//! Followups pre-filled from the previous meeting's minutes and
//! Tickets (and Membership) from the group's open GitLab issues.

use std::process::ExitCode;

use chrono::{NaiveDate, Weekday};
use sandogasa_gitlab::{GroupClient, Issue};
use sandogasa_meetbot::{Meetbot, Meeting};

use crate::list::DEFAULT_TOPIC;

/// The SIG's GitLab group, whose open issues are the meeting's tickets.
pub const GITLAB_URL: &str = "https://gitlab.com";
pub const GROUP: &str = "CentOS/Hyperscale";
/// Pagure imports, frozen read-only: listed nowhere.
const ARCHIVE_SUBGROUP: &str = "CentOS/Hyperscale/archive/";
/// hs-relmon's automated "new version available" issues are handled
/// out of band, not in the meeting.
const NEW_VERSION_LABEL: &str = "rfe::new-version";
/// Issues asked to be raised at a meeting.
const MEETING_LABEL: &str = "meeting";
/// Membership requests get their own topic.
const MEMBERSHIP_LABEL: &str = "membership";

#[derive(clap::Args)]
pub struct ScriptArgs {
    /// Meeting date (YYYY-MM-DD, default: the coming Wednesday).
    #[arg(long)]
    pub date: Option<NaiveDate>,

    /// Meetbot topic the previous meeting was recorded under.
    #[arg(short, long, default_value = DEFAULT_TOPIC)]
    pub topic: String,

    /// Emit JSON (the script plus what it was built from).
    #[arg(long)]
    pub json: bool,
}

/// The previous meeting, as far as Followups needs it.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct Previous {
    pub date: NaiveDate,
    pub summary_url: String,
    pub action_items: Vec<String>,
}

/// An open group issue on the agenda.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct Ticket {
    /// `tracker#156`: the project below the group, then the iid.
    pub reference: String,
    pub title: String,
    pub url: String,
    pub labels: Vec<String>,
    pub created: NaiveDate,
    /// Opened since the previous meeting.
    pub new: bool,
}

#[derive(serde::Serialize)]
struct ScriptJson<'a> {
    date: NaiveDate,
    previous: Option<&'a Previous>,
    tickets: &'a [Ticket],
    membership: &'a [Ticket],
    script: String,
}

pub fn run(args: &ScriptArgs) -> ExitCode {
    let date = args
        .date
        .unwrap_or_else(|| sandogasa_cli::date::next_weekday(today(), Weekday::Wed));
    // Both sources are best effort: a script with an empty section
    // still beats typing the template by hand.
    let meetbot = Meetbot::new().with_cache(env!("CARGO_PKG_NAME"), false);
    let previous = match previous_meeting(&meetbot, &args.topic, date) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("warning: no followups: {e}");
            None
        }
    };
    let since = previous.as_ref().map(|p| p.date);
    let (tickets, membership) = match open_issues() {
        Ok(issues) => agenda(issues, since),
        Err(e) => {
            eprintln!("warning: no tickets: {e}");
            (Vec::new(), Vec::new())
        }
    };
    let script = render_script(previous.as_ref(), &tickets, &membership);
    if args.json {
        let out = ScriptJson {
            date,
            previous: previous.as_ref(),
            tickets: &tickets,
            membership: &membership,
            script,
        };
        println!("{}", serde_json::to_string_pretty(&out).expect("serialize"));
    } else {
        // Context on stderr so stdout stays a clean, pipeable script
        // (`hs-meetings script > meeting.txt`).
        eprint!(
            "{}",
            checklist(date, previous.as_ref(), &tickets, &membership)
        );
        print!("{script}");
    }
    ExitCode::SUCCESS
}

/// The newest `topic` meeting recorded before `date`, with its
/// action items — Followups material.
pub fn previous_meeting(
    meetbot: &Meetbot,
    topic: &str,
    date: NaiveDate,
) -> Result<Option<Previous>, Box<dyn std::error::Error>> {
    let meetings = meetbot.search(topic)?;
    let Some(meeting) = meetings
        .iter()
        .rfind(|m: &&Meeting| m.topic == topic && m.datetime.date() < date)
    else {
        return Ok(None);
    };
    let minutes = meetbot.minutes(meeting)?;
    Ok(Some(Previous {
        date: meeting.datetime.date(),
        summary_url: meeting.summary_url.clone(),
        action_items: sandogasa_meetbot::parse_action_items(&minutes),
    }))
}

/// The group's open issues minus the automated new-version ones;
/// public, so no token.
fn open_issues() -> Result<Vec<Issue>, Box<dyn std::error::Error>> {
    GroupClient::new(GITLAB_URL, GROUP, "")?
        .list_issues_where(&[("state", "opened"), ("not[labels]", NEW_VERSION_LABEL)])
}

/// The Tickets and Membership lists: archived Pagure imports dropped,
/// `meeting`-labelled issues first, then newest first; membership
/// requests split out; `new` marks issues opened since `since`.
pub fn agenda(issues: Vec<Issue>, since: Option<NaiveDate>) -> (Vec<Ticket>, Vec<Ticket>) {
    let mut tickets: Vec<Ticket> = issues
        .into_iter()
        .filter(|i| !i.web_url.contains(&format!("/{ARCHIVE_SUBGROUP}")))
        .map(|i| {
            let created = i
                .created_at
                .as_deref()
                .and_then(|t| t.get(..10)?.parse().ok())
                .unwrap_or(NaiveDate::MIN);
            Ticket {
                reference: reference(&i.web_url, i.iid),
                title: i.title,
                url: i.web_url,
                labels: i.labels,
                created,
                new: since.is_some_and(|d| created > d),
            }
        })
        .collect();
    tickets.sort_by(|a, b| {
        let meeting = |t: &Ticket| t.labels.iter().any(|l| l == MEETING_LABEL);
        meeting(b).cmp(&meeting(a)).then(b.created.cmp(&a.created))
    });
    let (membership, tickets) = tickets
        .into_iter()
        .partition(|t| t.labels.iter().any(|l| l == MEMBERSHIP_LABEL));
    (tickets, membership)
}

/// `tracker#156` from the issue's URL: the project path below the
/// group, then the iid.
fn reference(web_url: &str, iid: u64) -> String {
    let project = web_url
        .split_once(&format!("/{GROUP}/"))
        .and_then(|(_, rest)| rest.split_once("/-/"))
        .map_or("?", |(project, _)| project);
    format!("{project}#{iid}")
}

/// The zodbot script, per the SIG's meeting checklist: Followups
/// link the previous minutes and carry its action items; Tickets link
/// the group's open-issue view and then each issue; Membership appears
/// only when a request is open.
pub fn render_script(
    previous: Option<&Previous>,
    tickets: &[Ticket],
    membership: &[Ticket],
) -> String {
    use std::fmt::Write as _;
    let mut o = String::from(
        "!startmeeting CentOS Hyperscale SIG\n\
         !topic Roll call\n\
         !topic Followups\n",
    );
    if let Some(p) = previous {
        let _ = writeln!(o, "!link {}", p.summary_url);
        for item in &p.action_items {
            let _ = writeln!(o, "!info followup from {}: {item}", p.date);
        }
    }
    let _ = writeln!(
        o,
        "!topic Announcements\n\
         !topic Tickets\n\
         !link {GITLAB_URL}/groups/{GROUP}/-/work_items?sort=created_date&state=opened&not[label_name][]={}",
        NEW_VERSION_LABEL.replace("::", "%3A%3A")
    );
    for t in tickets {
        let _ = writeln!(o, "!link {}", t.url);
    }
    if !membership.is_empty() {
        o.push_str("!topic Membership\n");
        for t in membership {
            let _ = writeln!(o, "!link {}", t.url);
        }
    }
    o.push_str("!topic Open Floor\n!endmeeting\n");
    o
}

/// What the chair needs to know alongside the script: the meeting's
/// date, what each linked ticket is, and the after-meeting step.
pub fn checklist(
    date: NaiveDate,
    previous: Option<&Previous>,
    tickets: &[Ticket],
    membership: &[Ticket],
) -> String {
    use std::fmt::Write as _;
    let mut o = format!("── CentOS Hyperscale SIG meeting ({date}) ──\n");
    match previous {
        Some(p) => {
            let _ = writeln!(
                o,
                "Followups: {} action item(s) from {} (see !info lines)",
                p.action_items.len(),
                p.date
            );
        }
        None => o.push_str("Followups: no previous meeting found\n"),
    }
    let width = tickets
        .iter()
        .chain(membership)
        .map(|t| t.reference.len())
        .max()
        .unwrap_or(0);
    for (name, list) in [("Tickets", tickets), ("Membership", membership)] {
        let _ = writeln!(o, "{name}: {}", list.len());
        for t in list {
            let _ = writeln!(
                o,
                "  {}{:<width$}  {}  [{}]",
                if t.new { "new " } else { "    " },
                t.reference,
                t.title,
                t.labels.join(", ")
            );
        }
    }
    o.push_str(
        "Paste the stdout lines into #meeting:fedoraproject.org as the meeting\n\
         progresses. Afterwards, from the docs checkout:\n\
         \x20 hs-meetings sync -f docs/communication/meetings-list.md\n\
         ────\n",
    );
    o
}

fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(project: &str, iid: u64, title: &str, labels: &[&str], created: &str) -> Issue {
        Issue {
            iid,
            title: title.into(),
            description: None,
            state: "opened".into(),
            web_url: format!("{GITLAB_URL}/{GROUP}/{project}/-/work_items/{iid}"),
            labels: labels.iter().map(|l| l.to_string()).collect(),
            assignees: vec![],
            start_date: None,
            due_date: None,
            created_at: Some(format!("{created}T10:00:00.000Z")),
        }
    }

    fn date(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn agenda_orders_splits_and_marks_new() {
        let issues = vec![
            issue("tracker", 115, "Btrfs images", &[], "2026-02-04"),
            issue(
                "tracker",
                157,
                "membership for aleivag",
                &["membership"],
                "2026-06-16",
            ),
            issue(
                "tracker",
                156,
                "Branch edk2",
                &["new-package"],
                "2026-09-01",
            ),
            issue(
                "tracker",
                8,
                "EROFS zstd",
                &["kernel", "meeting"],
                "2026-02-04",
            ),
            issue(
                "archive/pagure.io/x/package-bugs",
                55,
                "old",
                &["c8s"],
                "2026-02-04",
            ),
        ];
        let (tickets, membership) = agenda(issues, Some(date("2026-08-26")));
        let refs: Vec<&str> = tickets.iter().map(|t| t.reference.as_str()).collect();
        assert_eq!(refs, ["tracker#8", "tracker#156", "tracker#115"]);
        assert_eq!(
            tickets.iter().map(|t| t.new).collect::<Vec<_>>(),
            [false, true, false]
        );
        assert_eq!(membership.len(), 1);
        assert_eq!(membership[0].reference, "tracker#157");
    }

    #[test]
    fn render_script_follows_the_checklist() {
        let previous = Previous {
            date: date("2026-08-26"),
            summary_url: "https://meetbot.example/m.html".into(),
            action_items: vec!["Davide and Neal to submit the report".into()],
        };
        let (tickets, membership) = agenda(
            vec![
                issue(
                    "tracker",
                    156,
                    "Branch edk2",
                    &["new-package"],
                    "2026-06-02",
                ),
                issue("tracker", 157, "membership", &["membership"], "2026-06-16"),
            ],
            None,
        );
        let script = render_script(Some(&previous), &tickets, &membership);
        assert_eq!(
            script,
            "!startmeeting CentOS Hyperscale SIG\n\
             !topic Roll call\n\
             !topic Followups\n\
             !link https://meetbot.example/m.html\n\
             !info followup from 2026-08-26: Davide and Neal to submit the report\n\
             !topic Announcements\n\
             !topic Tickets\n\
             !link https://gitlab.com/groups/CentOS/Hyperscale/-/work_items?sort=created_date&state=opened&not[label_name][]=rfe%3A%3Anew-version\n\
             !link https://gitlab.com/CentOS/Hyperscale/tracker/-/work_items/156\n\
             !topic Membership\n\
             !link https://gitlab.com/CentOS/Hyperscale/tracker/-/work_items/157\n\
             !topic Open Floor\n\
             !endmeeting\n"
        );
    }

    #[test]
    fn render_script_without_sources_is_the_bare_template() {
        let script = render_script(None, &[], &[]);
        assert!(!script.contains("!topic Membership"));
        assert!(!script.contains("!info"));
        assert!(script.ends_with("!topic Open Floor\n!endmeeting\n"));
    }

    #[test]
    fn checklist_lists_tickets_with_their_labels() {
        let (tickets, membership) = agenda(
            vec![issue("tracker", 8, "EROFS zstd", &["kernel"], "2026-02-04")],
            None,
        );
        let out = checklist(date("2026-09-23"), None, &tickets, &membership);
        assert!(out.contains("(2026-09-23)"));
        assert!(out.contains("no previous meeting"));
        assert!(out.contains("tracker#8"));
        assert!(out.contains("EROFS zstd  [kernel]"));
        assert!(out.contains("hs-meetings sync"));
    }
}
