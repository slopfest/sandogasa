// SPDX-License-Identifier: Apache-2.0 OR MIT

//! How much of a package's recent history comes from people with
//! access to it (#60).
//!
//! When the maintainers of a package to rebuild have not pushed to it
//! for a long while, a ping may go unanswered, and the plan should not
//! wait long on it. dist-git has no commit log API, and a commit names
//! its author rather than a Fedora account, so the history is read from
//! the push messages Fedora's message bus keeps
//! (`org.fedoraproject.prod.pagure.git.receive`, through datagrepper).
//! Each names the account that pushed, or merged a pull request
//! (`agent`), and the project's access lists as they were at the time.
//! Release engineering's mass rebuilds (`releng`) are counted apart.
//!
//! Only counts and dates are reported, never names: this is a hint on
//! how long to wait for a reply, not a judgement of anyone.
//!
//! datagrepper answers a search on one package, newest first and
//! bounded in time, in seconds; an unbounded or oldest-first one runs
//! past a minute and fails. So each package is one such search, in a
//! window of [`WINDOW_DAYS`], for at most [`MAX_PUSHES`] pushes.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

/// datagrepper.
pub const DATAGREPPER_BASE: &str = "https://apps.fedoraproject.org/datagrepper";

/// The topic dist-git's pushes are sent under.
pub const TOPIC: &str = "org.fedoraproject.prod.pagure.git.receive";

/// How far back to look.
pub const WINDOW_DAYS: i64 = 730;

/// The most recent pushes to count.
pub const MAX_PUSHES: usize = 20;

/// Pages of messages to read at most: forks and other branches share
/// the search, so a page can hold few pushes that count.
const MAX_PAGES: u64 = 3;

/// The account release engineering's mass rebuilds push as.
pub const MASS_REBUILD_AGENT: &str = "releng";

/// The roles that can push to every branch. A collaborator can push
/// only to the branches their pattern names, and ticket access cannot
/// push at all.
const PUSH_ROLES: [&str; 3] = ["owner", "admin", "commit"];

/// How datagrepper is retried: it answers gateway errors under load.
/// Two attempts, as a package's history is a hint the plan does without
/// when it cannot be had.
const RETRY: sandogasa_cli::retry::Policy = sandogasa_cli::retry::Policy::attempts(2);

/// One push to the package's branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Push {
    pub at: DateTime<Utc>,
    /// The account that pushed, or merged the pull request.
    pub agent: String,
    /// The users with access at the time.
    pub users: BTreeSet<String>,
    /// The groups with access at the time.
    pub groups: BTreeSet<String>,
}

/// A package's latest pushes to a branch, as read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pushes {
    /// Newest first, at most [`MAX_PUSHES`].
    pub list: Vec<Push>,
    /// When the search stopped at its page limit with fewer than
    /// [`MAX_PUSHES`] found: the oldest message it read, which is as far
    /// back as the count goes.
    pub read_back_to: Option<DateTime<Utc>>,
}

/// What a package's recent pushes say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct History {
    /// The branch counted.
    pub branch: String,
    /// The pushes counted, at most [`MAX_PUSHES`].
    pub pushes: usize,
    /// Of those, the ones by an account with access.
    pub by_access: usize,
    /// Of those, release engineering's mass rebuilds.
    pub mass_rebuilds: usize,
    /// The date the count goes back to: the oldest push counted when
    /// there were [`MAX_PUSHES`], else the oldest message read when the
    /// search stopped at its page limit, else the window's start.
    pub since: String,
    /// The date of the latest push by an account with access, when
    /// one is among those counted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_by_access: Option<String>,
}

/// Whether a collaborator's branch scope (`epel*`, or several patterns
/// separated by commas) covers `branch`.
pub fn branch_in_scope(patterns: &str, branch: &str) -> bool {
    patterns
        .split(',')
        .map(str::trim)
        .any(|p| !p.is_empty() && crate::check_crate::glob_match(p, branch))
}

/// The collaborators, `(name, branch scope)`, whose scope covers
/// `branch`.
pub fn collaborators_on(collaborators: &[(String, Option<String>)], branch: &str) -> Vec<String> {
    collaborators
        .iter()
        .filter(|(_, scope)| scope.as_deref().is_some_and(|s| branch_in_scope(s, branch)))
        .map(|(name, _)| name.clone())
        .collect()
}

/// The names in a message's access list that could push to `branch`:
/// owners, admins and committers, and collaborators whose branch scope
/// the message gives and covers it. Messages list collaborators by name
/// alone, so those are left to [`collaborators_on`], from dist-git.
fn access_names(v: &Value, branch: &str) -> BTreeSet<String> {
    let full = PUSH_ROLES
        .iter()
        .filter_map(|role| v.get(*role)?.as_array())
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_string));
    let scoped = v["collaborator"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| {
            let name = x.get("user").or_else(|| x.get("name"))?.as_str()?;
            branch_in_scope(x.get("branches")?.as_str()?, branch).then(|| name.to_string())
        });
    full.chain(scoped).collect()
}

/// When a message was sent.
fn sent_at(m: &Value) -> Option<DateTime<Utc>> {
    m["headers"]["sent-at"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
        .or_else(|| {
            m["timestamp"]
                .as_f64()
                .and_then(|t| DateTime::from_timestamp(t as i64, 0))
        })
}

/// One page of a datagrepper search.
#[derive(Debug, Default)]
pub struct Page {
    /// The pushes to the package's branch.
    pub pushes: Vec<Push>,
    /// How many pages the search has.
    pub pages: u64,
    /// The oldest message on the page, counted or not.
    pub oldest: Option<DateTime<Utc>>,
}

/// The pushes to `rpms/<package>`'s `branch` in one page of a
/// datagrepper search. Pushes to forks and other branches are left
/// out, as is the one creating the branch (releng's, at branching),
/// which lists the whole history as new.
pub fn parse_page(json: &str, package: &str, branch: &str) -> Result<Page, String> {
    let v: Value =
        serde_json::from_str(json).map_err(|e| format!("cannot read datagrepper's answer: {e}"))?;
    let pages = v.get("pages").and_then(Value::as_u64).unwrap_or(1);
    let project = format!("rpms/{package}");
    let messages = v["raw_messages"].as_array().cloned().unwrap_or_default();
    let zeros = |c: &Value| {
        c.as_str()
            .is_some_and(|c| !c.is_empty() && c.bytes().all(|x| x == b'0'))
    };
    let pushes = messages
        .iter()
        .filter_map(|m| {
            let b = &m["body"];
            if b["repo"]["fullname"].as_str()? != project || b["branch"].as_str()? != branch {
                return None;
            }
            if zeros(&b["old_commit"]) {
                return None;
            }
            let at = sent_at(m)?;
            Some(Push {
                at,
                agent: b["agent"].as_str()?.to_string(),
                users: access_names(&b["repo"]["access_users"], branch),
                groups: access_names(&b["repo"]["access_groups"], branch),
            })
        })
        .collect();
    Ok(Page {
        pushes,
        pages,
        oldest: messages.iter().filter_map(sent_at).min(),
    })
}

/// The groups whose members are needed to tell whether `pushes` were
/// by people with access: those of pushes whose agent had no access as
/// a user and is not release engineering.
pub fn groups(pushes: &[Push]) -> BTreeSet<String> {
    pushes
        .iter()
        .filter(|p| !p.users.contains(&p.agent) && p.agent != MASS_REBUILD_AGENT)
        .flat_map(|p| p.groups.iter().cloned())
        .collect()
}

/// What `pushes` say, with `members` giving each group's members. A
/// push counts as by someone with access when its agent was a user with
/// access then, or is a member of a group with access then. Group
/// members are as they are now, not as they were.
pub fn summarize(
    pushes: &Pushes,
    branch: &str,
    members: &BTreeMap<String, Vec<String>>,
    window_start: DateTime<Utc>,
) -> History {
    let date = |d: &DateTime<Utc>| d.format("%Y-%m-%d").to_string();
    let had_access = |p: &Push| {
        p.users.contains(&p.agent)
            || p.groups
                .iter()
                .any(|g| members.get(g).is_some_and(|m| m.contains(&p.agent)))
    };
    let counted = &pushes.list;
    let with_access: Vec<&Push> = counted.iter().filter(|p| had_access(p)).collect();
    History {
        branch: branch.to_string(),
        pushes: counted.len(),
        by_access: with_access.len(),
        mass_rebuilds: counted
            .iter()
            .filter(|p| p.agent == MASS_REBUILD_AGENT)
            .count(),
        since: date(&match (counted.last(), pushes.read_back_to) {
            (Some(p), _) if counted.len() >= MAX_PUSHES => p.at,
            (_, Some(read)) => read,
            _ => window_start,
        }),
        latest_by_access: with_access.iter().map(|p| p.at).max().as_ref().map(date),
    }
}

/// The search URL for one page.
pub fn search_url(base: &str, package: &str, page: u64) -> String {
    format!(
        "{}/v2/search?topic={TOPIC}&package={}&order=desc&rows_per_page=100&page={page}&delta={}",
        base.trim_end_matches('/'),
        crate::discover::urlencode(package),
        WINDOW_DAYS * 86400
    )
}

/// One page, asked again once after a gateway error.
async fn get_page(http: &reqwest::Client, url: &str, package: &str) -> Result<String, String> {
    let resp = sandogasa_cli::retry::send("datagrepper", &RETRY, || http.get(url))
        .await
        .map_err(|e| format!("cannot reach datagrepper: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "datagrepper answered HTTP {} for {package}'s pushes",
            resp.status().as_u16()
        ));
    }
    resp.text()
        .await
        .map_err(|e| format!("cannot read datagrepper's answer: {e}"))
}

/// The latest pushes to `rpms/<package>`'s `branch`, newest first, at
/// most [`MAX_PUSHES`], read from the datagrepper at `base`.
pub async fn fetch(
    http: &reqwest::Client,
    base: &str,
    package: &str,
    branch: &str,
) -> Result<Pushes, String> {
    let mut list = Vec::new();
    let mut oldest = None;
    let mut page = 1;
    let read_back_to = loop {
        let text = get_page(http, &search_url(base, package, page), package).await?;
        let found = parse_page(&text, package, branch)?;
        list.extend(found.pushes);
        oldest = found.oldest.or(oldest);
        if list.len() >= MAX_PUSHES || page >= found.pages {
            break None;
        }
        if page >= MAX_PAGES {
            break oldest;
        }
        page += 1;
    };
    list.sort_by_key(|p| std::cmp::Reverse(p.at));
    list.truncate(MAX_PUSHES);
    Ok(Pushes { list, read_back_to })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message(at: &str, fullname: &str, branch: &str, agent: &str, old: &str) -> Value {
        json!({
            "headers": {"sent-at": at},
            "body": {
                "agent": agent,
                "branch": branch,
                "old_commit": old,
                "repo": {
                    "fullname": fullname,
                    "access_users": {"owner": ["aarem"], "admin": [], "commit": [],
                                     "collaborator": [], "ticket": []},
                    "access_groups": {"admin": [], "commit": ["kde-sig"],
                                      "collaborator": [], "ticket": []}
                }
            }
        })
    }

    /// osmo's pushes as datagrepper had them on 2026-11-10, trimmed: a
    /// merged pull request, a fork's new branch, a mass rebuild, a
    /// provenpackager's fix, one from its owner, and (made up, as a
    /// stand-in for branching) a push creating the branch.
    fn osmo_page() -> String {
        json!({
            "pages": 1,
            "raw_messages": [
                message("2026-11-05T10:00:00+00:00", "rpms/osmo", "rawhide", "salimma", "a1a36643"),
                message("2026-11-04T10:00:00+00:00", "forks/salimma/rpms/osmo", "rawhide-libical4",
                        "salimma", "0000000000000000000000000000000000000000"),
                message("2026-09-23T10:00:00+00:00", "rpms/osmo", "f45", "zbyszek", "400a1c42"),
                message("2026-08-12T10:00:00+00:00", "rpms/osmo", "rawhide", "releng",
                        "0000000000000000000000000000000000000000"),
                message("2026-07-16T10:00:00+00:00", "rpms/osmo", "rawhide", "releng", "7d41dd63"),
                message("2026-02-25T10:00:00+00:00", "rpms/osmo", "rawhide", "filiperosset", "5747aa8b"),
                message("2025-03-01T10:00:00+00:00", "rpms/osmo", "rawhide", "aarem", "e91acb51"),
            ]
        })
        .to_string()
    }

    #[test]
    fn only_pushes_to_the_packages_own_branch_count() {
        let page = parse_page(&osmo_page(), "osmo", "rawhide").unwrap();
        assert_eq!(page.pages, 1);
        let agents: Vec<&str> = page.pushes.iter().map(|p| p.agent.as_str()).collect();
        assert_eq!(agents, ["salimma", "releng", "filiperosset", "aarem"]);
        assert_eq!(
            page.oldest.unwrap().format("%Y-%m-%d").to_string(),
            "2025-03-01"
        );
        // Only the pushes by someone with no access as a user, other than
        // mass rebuilds, need a group's members.
        assert_eq!(
            groups(&page.pushes),
            BTreeSet::from(["kde-sig".to_string()])
        );
        assert!(groups(&page.pushes[3..]).is_empty(), "aarem owns it");
        assert!(groups(&page.pushes[1..2]).is_empty(), "releng");
    }

    #[test]
    fn pushes_by_access_mass_rebuilds_and_the_latest_with_access() {
        let pushes = Pushes {
            list: parse_page(&osmo_page(), "osmo", "rawhide").unwrap().pushes,
            read_back_to: None,
        };
        let start = DateTime::parse_from_rfc3339("2024-11-10T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let h = summarize(&pushes, "rawhide", &BTreeMap::new(), start);
        assert_eq!(
            h,
            History {
                branch: "rawhide".into(),
                pushes: 4,
                by_access: 1,
                mass_rebuilds: 1,
                since: "2024-11-10".into(),
                latest_by_access: Some("2025-03-01".into()),
            }
        );
        // A member of a group with access counts too.
        let members = BTreeMap::from([("kde-sig".to_string(), vec!["filiperosset".to_string()])]);
        let h = summarize(&pushes, "rawhide", &members, start);
        assert_eq!(h.by_access, 2);
        assert_eq!(h.latest_by_access.as_deref(), Some("2026-02-25"));
        // A search cut short goes back only as far as it read.
        let short = Pushes {
            read_back_to: Some(pushes.list[2].at),
            ..pushes.clone()
        };
        let h = summarize(&short, "rawhide", &BTreeMap::new(), start);
        assert_eq!(h.since, "2026-02-25");
        // No pushes: the window's start, and no latest.
        let none = summarize(&Pushes::default(), "rawhide", &BTreeMap::new(), start);
        assert_eq!((none.pushes, none.since.as_str()), (0, "2024-11-10"));
        assert_eq!(none.latest_by_access, None);
    }

    #[test]
    fn only_roles_that_can_push_to_the_branch_count_as_access() {
        let v = json!({"owner": ["a"], "ticket": ["c"],
                       "collaborator": [{"user": "b", "branches": "f*"},
                                        {"user": "e", "branches": "epel*"}, "d"]});
        assert_eq!(
            access_names(&v, "f45"),
            BTreeSet::from(["a".to_string(), "b".to_string()])
        );
        assert_eq!(
            access_names(&v, "rawhide"),
            BTreeSet::from(["a".to_string()])
        );
        assert!(branch_in_scope("epel*, f45", "f45"));
        assert!(!branch_in_scope("epel*", "rawhide"));
        let collaborators = [
            ("epel-only".to_string(), Some("epel*".to_string())),
            ("everywhere".to_string(), Some("*".to_string())),
            ("unscoped".to_string(), None),
        ];
        assert_eq!(
            collaborators_on(&collaborators, "epel10"),
            ["epel-only", "everywhere"]
        );
        assert_eq!(collaborators_on(&collaborators, "rawhide"), ["everywhere"]);
    }

    /// One bounded search per package, read page by page.
    #[test]
    fn fetch_reads_pages_until_it_has_enough() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = rt.block_on(MockServer::start());
        let page = |at: &str| {
            json!({"pages": 2, "raw_messages": [
                message(at, "rpms/osmo", "rawhide", "aarem", "1234")
            ]})
            .to_string()
        };
        rt.block_on(async {
            // datagrepper under load: one 502, then the answer.
            Mock::given(method("GET"))
                .and(path("/v2/search"))
                .and(query_param("page", "1"))
                .respond_with(ResponseTemplate::new(502))
                .up_to_n_times(1)
                .expect(1)
                .mount(&server)
                .await;
            for (n, at) in [(1, "2026-11-01T00:00:00Z"), (2, "2026-01-01T00:00:00Z")] {
                Mock::given(method("GET"))
                    .and(path("/v2/search"))
                    .and(query_param("package", "osmo"))
                    .and(query_param("topic", TOPIC))
                    .and(query_param("order", "desc"))
                    .and(query_param("page", n.to_string()))
                    .respond_with(ResponseTemplate::new(200).set_body_string(page(at)))
                    .expect(1)
                    .mount(&server)
                    .await;
            }
        });
        sandogasa_cli::init();
        let http = reqwest::Client::new();
        let pushes = rt
            .block_on(fetch(&http, &server.uri(), "osmo", "rawhide"))
            .unwrap();
        assert_eq!(pushes.list.len(), 2);
        assert!(pushes.list[0].at > pushes.list[1].at, "newest first");
        assert_eq!(pushes.read_back_to, None, "every page was read");
        assert!(search_url(DATAGREPPER_BASE, "osmo", 1).contains("&delta=63072000"));
        assert!(
            search_url(DATAGREPPER_BASE, "libsigc++20", 1).contains("package=libsigc%2B%2B20&")
        );
    }
}
