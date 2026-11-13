// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Which packages a collection's tags list. CBS builds a package into
//! a tag only once it is listed there, with an owner: a first build of
//! a package needs `cbs add-pkg --owner=<user> <tag> <package>` for each
//! of the collection's `-candidate`, `-testing` and `-release` tags
//! first.

use std::process::{Command, Stdio};

use crate::{Collection, PROFILE, Stage};

/// The tags a collection's packages have to be listed in, in the order
/// a build moves through them.
pub fn stage_tags(collection: &Collection) -> [String; 3] {
    [Stage::Candidate, Stage::Testing, Stage::Release].map(|s| collection.tag(s))
}

/// The command that lists `package` in `tag`, as a person would type
/// it.
pub fn add_pkg_argv(owner: &str, tag: &str, package: &str) -> Vec<String> {
    vec![
        "cbs".to_string(),
        "add-pkg".to_string(),
        format!("--owner={owner}"),
        tag.to_string(),
        package.to_string(),
    ]
}

/// The CBS user koji's `moshimoshi` greets: `salimma` from
/// `what's up, salimma!` (the greeting word varies, the name follows
/// the last comma).
pub fn greeted_user(moshimoshi: &str) -> Option<String> {
    let first = moshimoshi.lines().map(str::trim).find(|l| !l.is_empty())?;
    let (_, name) = first.rsplit_once(", ")?;
    let name = name.trim_end_matches('!').trim();
    (!name.is_empty() && !name.contains(' ')).then(|| name.to_string())
}

/// Whether `koji list-pkgs --quiet` output, with its exit status,
/// lists `package`: `None` when it is an error rather than an answer.
pub fn listed_in(output: &str, success: bool, package: &str) -> Option<bool> {
    if success {
        return Some(
            output
                .lines()
                .any(|l| l.split_whitespace().next() == Some(package)),
        );
    }
    output.contains("(no matching packages)").then_some(false)
}

/// The furthest stage of `collection` that `tags` (a build's) include.
pub fn furthest_stage(collection: &Collection, tags: &[String]) -> Option<Stage> {
    [Stage::Release, Stage::Testing, Stage::Candidate]
        .into_iter()
        .find(|s| tags.iter().any(|t| *t == collection.tag(*s)))
}

/// The commands that promote `nvr` in `collection` to `to`: tag it
/// there unless `tags` (its tags) include it already, and, for a
/// release, untag it from `-testing` when `tags` include that, so a
/// released build does not stay in testing as well. Untagging was forgotten often enough by hand that it is part
/// of the step, for every SIG.
pub fn promote_argvs(
    collection: &Collection,
    nvr: &str,
    to: Stage,
    tags: &[String],
) -> Vec<Vec<String>> {
    let cbs = |args: &[&str]| -> Vec<String> {
        std::iter::once("cbs")
            .chain(args.iter().copied())
            .map(String::from)
            .collect()
    };
    let target = collection.tag(to);
    let mut argvs = Vec::new();
    // A build released before untagging was part of the step is in
    // -release already: only the untag is left.
    if !tags.contains(&target) {
        argvs.push(cbs(&["tag-build", &target, nvr]));
    }
    let testing = collection.tag(Stage::Testing);
    if to == Stage::Release && tags.contains(&testing) {
        argvs.push(cbs(&["untag-build", &testing, nvr]));
    }
    argvs
}

/// The tags `nvr` is in.
pub fn tags_of(nvr: &str) -> Result<Vec<String>, String> {
    let out = Command::new("koji")
        .args(["--profile", PROFILE, "list-tags", "--build", nvr])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run koji: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "koji list-tags --build {nvr}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// The commit a build was made from: the `#<sha>` ending the `Source:`
/// line `koji buildinfo` prints, as for
/// `git+https://gitlab.com/CentOS/proposed_updates/rpms/git.git#a879194…`.
pub fn source_commit(buildinfo: &str) -> Option<String> {
    let source = buildinfo
        .lines()
        .find_map(|l| l.strip_prefix("Source:"))?
        .trim();
    let (_, commit) = source.rsplit_once('#')?;
    (!commit.is_empty() && commit.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| commit.to_string())
}

/// The commit `nvr` was built from in CBS, if its source names one.
pub fn build_commit(nvr: &str) -> Result<Option<String>, String> {
    let out = Command::new("koji")
        .args(["--profile", PROFILE, "buildinfo", nvr])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run koji: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "koji buildinfo {nvr}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(source_commit(&String::from_utf8_lossy(&out.stdout)))
}

/// The build a finished build task made, from the `Build: <nvr> (<id>)`
/// line `koji taskinfo` prints.
pub fn task_build_of(taskinfo: &str) -> Option<String> {
    taskinfo
        .lines()
        .find_map(|l| l.strip_prefix("Build:"))?
        .split_whitespace()
        .next()
        .map(str::to_string)
}

/// The build CBS task `task` made, if it made one.
pub fn task_build(task: &str) -> Result<Option<String>, String> {
    let out = Command::new("koji")
        .args(["--profile", PROFILE, "taskinfo", task])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run koji: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "koji taskinfo {task}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(task_build_of(&String::from_utf8_lossy(&out.stdout)))
}

/// The newest build of `package` in `tag`, if it has one.
pub fn latest(tag: &str, package: &str) -> Option<String> {
    let out = Command::new("koji")
        .args([
            "--profile",
            PROFILE,
            "list-tagged",
            "--latest",
            "--quiet",
            tag,
            package,
        ])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
}

/// The newest build of `package` in each of `tags`, in one request to
/// the CBS hub (`koji --profile cbs`'s, else CBS's own): one entry per
/// tag, `None` where the tag has no build of it.
pub fn latest_in_tags(tags: &[String], package: &str) -> Result<Vec<Option<String>>, String> {
    let hub_url =
        sandogasa_kojihub::config::hub_url(PROFILE).unwrap_or_else(|| crate::HUB_URL.to_string());
    latest_in_tags_at(&hub_url, tags, package)
}

/// [`latest_in_tags`], asking the hub at `hub_url`.
pub fn latest_in_tags_at(
    hub_url: &str,
    tags: &[String],
    package: &str,
) -> Result<Vec<Option<String>>, String> {
    let found = sandogasa_kojihub::HubClient::new(hub_url)
        .latest_in_tags(tags, package)
        .map_err(|e| e.to_string())?;
    if found.len() != tags.len() {
        return Err(format!(
            "the hub answered {} of {} tags",
            found.len(),
            tags.len()
        ));
    }
    found
        .into_iter()
        .zip(tags)
        .map(|(r, tag)| {
            r.map(|b| b.map(|b| b.nvr))
                .map_err(|e| format!("{tag}: {e}"))
        })
        .collect()
}

/// The CBS user the login belongs to, as koji's `moshimoshi` greets it.
pub fn whoami() -> Result<String, String> {
    let out = Command::new("koji")
        .args(["--profile", PROFILE, "moshimoshi"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run koji: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    greeted_user(&text).ok_or_else(|| {
        format!(
            "could not tell the CBS user from `koji --profile {PROFILE} moshimoshi`: {}",
            text.lines().next().unwrap_or("no output")
        )
    })
}

/// Whether `tag` lists `package` (inherited listings count).
pub fn listed(tag: &str, package: &str) -> Result<bool, String> {
    let out = Command::new("koji")
        .args([
            "--profile",
            PROFILE,
            "list-pkgs",
            "--quiet",
            "--tag",
            tag,
            "--package",
            package,
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run koji: {e}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    listed_in(&text, out.status.success(), package).ok_or_else(|| {
        format!(
            "koji list-pkgs --tag {tag} --package {package}: {}",
            text.trim()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::FakeTools;

    #[test]
    fn a_packages_newest_build_is_read_from_each_tag_at_once() {
        let build = "<value><array><data><value><array><data><value><struct>\
             <member><name>nvr</name><value><string>ethtool-7.1-1.hs.el10</string></value></member>\
             </struct></value></data></array></value></data></array></value>";
        let none = "<value><array><data><value><array><data></data></array></value></data></array></value>";
        let answer = |inner: &str| {
            format!(
                "<?xml version='1.0'?><methodResponse><params><param>\
                 <value><array><data>{inner}</data></array></value>\
                 </param></params></methodResponse>"
            )
        };
        let mut server = mockito::Server::new();
        let hub = server
            .mock("POST", "/")
            .with_status(200)
            .with_body(answer(&format!("{build}{none}")))
            .expect(1)
            .create();
        let url = server.url();
        let tags = [
            "hyperscale10s-packages-main-release".to_string(),
            "hyperscale9s-packages-main-release".to_string(),
        ];
        assert_eq!(
            latest_in_tags_at(&url, &tags, "ethtool").unwrap(),
            [Some("ethtool-7.1-1.hs.el10".to_string()), None]
        );
        hub.assert();
        // An answer for fewer tags than asked is an error, not "the
        // rest have none".
        server
            .mock("POST", "/")
            .with_status(200)
            .with_body(answer(build))
            .create();
        assert!(latest_in_tags_at(&url, &tags, "ethtool").is_err());
        // A hub that cannot be reached is an error, not "no builds".
        assert!(latest_in_tags_at("http://127.0.0.1:1", &tags, "ethtool").is_err());
    }

    #[test]
    #[serial_test::serial]
    fn a_builds_tags_are_read_from_koji() {
        let t = FakeTools::new();
        t.fake(
            "koji",
            &[
                ("list-tags --build xz-1", "a-candidate\n\n  a-testing  \n"),
                ("list-tags", "!GenericError: No such build"),
            ],
        );
        assert_eq!(tags_of("xz-1").unwrap(), ["a-candidate", "a-testing"]);
        let err = tags_of("xz-2").unwrap_err();
        assert!(
            err.contains("list-tags --build xz-2: GenericError"),
            "{err}"
        );
        std::fs::remove_file(t.tmp.path().join("bin/koji")).unwrap();
        assert!(tags_of("xz-1").unwrap_err().contains("cannot run koji"));
    }

    #[test]
    #[serial_test::serial]
    fn a_builds_commit_is_read_from_koji() {
        let t = FakeTools::new();
        t.fake(
            "koji",
            &[
                (
                    "--profile cbs buildinfo git-2.52.1-1~proposed.el10",
                    "BUILD: git-2.52.1-1~proposed.el10 [80253]\nSource: git+https://gitlab.com/CentOS/proposed_updates/rpms/git.git#a87919483542fccf5821e798aa061e51f6501ede",
                ),
                ("buildinfo xz-1", "BUILD: xz-1\nSource: cli-build/xz.src.rpm"),
                ("buildinfo", "!GenericError: No such build"),
            ],
        );
        assert_eq!(
            build_commit("git-2.52.1-1~proposed.el10")
                .unwrap()
                .as_deref(),
            Some("a87919483542fccf5821e798aa061e51f6501ede")
        );
        assert_eq!(build_commit("xz-1").unwrap(), None);
        let err = build_commit("xz-2").unwrap_err();
        assert!(err.contains("buildinfo xz-2: GenericError"), "{err}");
        std::fs::remove_file(t.tmp.path().join("bin/koji")).unwrap();
        assert!(
            build_commit("xz-1")
                .unwrap_err()
                .contains("cannot run koji")
        );
    }

    #[test]
    #[serial_test::serial]
    fn a_tasks_build_is_read_from_koji() {
        let t = FakeTools::new();
        t.fake(
            "koji",
            &[
                (
                    "--profile cbs taskinfo 6093703",
                    "Task: 6093703\nType: build\nBuild: git-2.52.1-1~proposed.el10 (80253)\nLog Files:",
                ),
                ("taskinfo 2", "Task: 2\nType: buildArch"),
                ("taskinfo", "!GenericError: No such task"),
            ],
        );
        assert_eq!(
            task_build("6093703").unwrap().as_deref(),
            Some("git-2.52.1-1~proposed.el10")
        );
        assert_eq!(task_build("2").unwrap(), None);
        assert!(
            task_build("3")
                .unwrap_err()
                .contains("taskinfo 3: GenericError")
        );
        std::fs::remove_file(t.tmp.path().join("bin/koji")).unwrap();
        assert!(task_build("2").unwrap_err().contains("cannot run koji"));
    }

    #[test]
    #[serial_test::serial]
    fn the_newest_build_in_a_tag_is_its_first_word() {
        let t = FakeTools::new();
        t.fake(
            "koji",
            &[
                ("--latest --quiet t xz", "xz-5.4-1.el10  t  salimma"),
                ("list-tagged", ""),
            ],
        );
        assert_eq!(latest("t", "xz").as_deref(), Some("xz-5.4-1.el10"));
        assert_eq!(latest("t", "rsync"), None);
    }

    #[test]
    #[serial_test::serial]
    fn the_cbs_user_is_who_koji_greets() {
        let t = FakeTools::new();
        t.fake(
            "koji",
            &[("--profile cbs moshimoshi", "what's up, tester!")],
        );
        assert_eq!(whoami().as_deref(), Ok("tester"));
        t.fake("koji", &[("moshimoshi", "Hello!")]);
        let err = whoami().unwrap_err();
        assert!(
            err.contains("could not tell the CBS user") && err.contains("Hello!"),
            "{err}"
        );
        t.fake("koji", &[]);
        assert!(whoami().unwrap_err().contains("no output"));
    }

    #[test]
    #[serial_test::serial]
    fn whether_a_tag_lists_a_package_is_asked_of_koji() {
        let t = FakeTools::new();
        t.fake(
            "koji",
            &[
                ("--tag listed --package xz", "xz  listed  salimma"),
                ("--tag unlisted", "!(no matching packages)"),
                ("--tag broken", "!GenericError: no such tag"),
            ],
        );
        assert_eq!(listed("listed", "xz"), Ok(true));
        assert_eq!(listed("unlisted", "xz"), Ok(false));
        let err = listed("broken", "xz").unwrap_err();
        assert!(
            err.contains("--tag broken --package xz: GenericError"),
            "{err}"
        );
    }

    #[test]
    fn a_build_is_promoted_a_stage_at_a_time_and_released_out_of_testing() {
        let c = crate::Sig::ProposedUpdates
            .collection_for_branch("c10s-proposed-RHEL-1")
            .unwrap();
        let tags = |ss: &[Stage]| ss.iter().map(|s| c.tag(*s)).collect::<Vec<_>>();
        let nvr = "blktrace-1.3.0-13~proposed.el10";
        let lines = |to, t: &[String]| {
            promote_argvs(&c, nvr, to, t)
                .iter()
                .map(|a| a.join(" "))
                .collect::<Vec<_>>()
        };

        let candidate = tags(&[Stage::Candidate]);
        assert_eq!(furthest_stage(&c, &candidate), Some(Stage::Candidate));
        assert_eq!(Stage::Candidate.next(), Some(Stage::Testing));
        assert_eq!(
            lines(Stage::Testing, &candidate),
            [format!(
                "cbs tag-build proposed_updates10s-packages-main-testing {nvr}"
            )]
        );

        let testing = tags(&[Stage::Candidate, Stage::Testing]);
        assert_eq!(furthest_stage(&c, &testing), Some(Stage::Testing));
        assert_eq!(
            lines(Stage::Release, &testing),
            [
                format!("cbs tag-build proposed_updates10s-packages-main-release {nvr}"),
                format!("cbs untag-build proposed_updates10s-packages-main-testing {nvr}"),
            ]
        );
        // Straight from -candidate: nothing to untag.
        assert_eq!(lines(Stage::Release, &candidate).len(), 1);

        let released = tags(&[Stage::Candidate, Stage::Testing, Stage::Release]);
        assert_eq!(furthest_stage(&c, &released), Some(Stage::Release));
        // Released but left in testing, as blktrace's was: only the
        // untag remains.
        assert_eq!(
            lines(Stage::Release, &released),
            [format!(
                "cbs untag-build proposed_updates10s-packages-main-testing {nvr}"
            )]
        );
        assert_eq!(Stage::Release.next(), None);
        assert_eq!(furthest_stage(&c, &["other-tag".to_string()]), None);
    }

    #[test]
    fn the_cbs_user_is_read_from_the_greeting() {
        let out =
            "what's up, salimma!\n\nYou are using the hub at https://cbs.centos.org/kojihub/\n";
        assert_eq!(greeted_user(out).as_deref(), Some("salimma"));
        assert_eq!(greeted_user("olá, someone!\n").as_deref(), Some("someone"));
        assert_eq!(greeted_user("Hello!\n"), None);
        assert_eq!(greeted_user(""), None);
    }

    #[test]
    fn a_listing_is_told_from_an_absence_and_an_error() {
        let xz = "xz    proposed_updates10s-packages-main-testing    salimma\n";
        assert_eq!(listed_in(xz, true, "xz"), Some(true));
        assert_eq!(listed_in(xz, true, "x"), Some(false));
        assert_eq!(
            listed_in("(no matching packages)\n", false, "rsync"),
            Some(false)
        );
        assert_eq!(
            listed_in("GenericError: no such tag\n", false, "rsync"),
            None
        );
    }

    #[test]
    fn a_collections_stage_tags_and_the_command_that_lists_a_package() {
        let c = crate::Sig::ProposedUpdates
            .collection_for_branch("c10s-proposed-RHEL-303721")
            .unwrap();
        assert_eq!(
            stage_tags(&c),
            [
                "proposed_updates10s-packages-main-candidate",
                "proposed_updates10s-packages-main-testing",
                "proposed_updates10s-packages-main-release",
            ]
        );
        assert_eq!(
            add_pkg_argv(
                "salimma",
                "proposed_updates10s-packages-main-candidate",
                "rsync"
            )
            .join(" "),
            "cbs add-pkg --owner=salimma proposed_updates10s-packages-main-candidate rsync"
        );
    }

    #[test]
    fn a_builds_commit_is_read_from_its_source() {
        let info = "BUILD: git-2.52.1-1~proposed.el10 [80253]\nState: COMPLETE\n\
            Source: git+https://gitlab.com/CentOS/proposed_updates/rpms/git.git#a87919483542fccf5821e798aa061e51f6501ede\n\
            Volume: DEFAULT\n";
        assert_eq!(
            source_commit(info).as_deref(),
            Some("a87919483542fccf5821e798aa061e51f6501ede")
        );
        // An SRPM build, or no source at all, names no commit.
        assert_eq!(
            source_commit("Source: cli-build/1700000000.1.abc/git.src.rpm\n"),
            None
        );
        assert_eq!(source_commit("Source: x.git#main\n"), None);
        assert_eq!(source_commit("State: COMPLETE\n"), None);
    }
}
