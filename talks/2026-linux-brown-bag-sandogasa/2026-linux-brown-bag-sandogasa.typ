// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// Linux brown bag, 2026-11-13. Build with:
//   typst compile 2026-linux-brown-bag-sandogasa.typ

#import "@preview/diatypst:0.9.1": *

#show: slides.with(
  title: "Packaging with Sandogasa",
  subtitle: "CentOS SIGs and EPEL",
  date: "2026.11.13",
  authors: ("Michel Lind"),
  ratio: 16 / 9,
  layout: "medium",
  title-color: rgb("#a14f8c"),
  toc: true,
)

#set quote(block: true)

= What sandogasa is

== Sandogasa in one slide

#grid(
  columns: (3fr, 1fr),
  gutter: 1em,
  [
    - A Rust workspace of *17 tools* and *29 library crates* for distro
      packaging work: Fedora, EPEL, CentOS SIGs, and Debian
    - Each tool does one job a packager used to do by hand, against the
      real services: Koji, Bodhi, Bugzilla, GitLab, Jira, CBS
    - Packaged in Fedora and EPEL: `dnf install sandogasa`; or
      `cargo install <tool>` from crates.io
    - Source, issues and docs:
      #link("https://github.com/slopfest/sandogasa")[github.com/slopfest/sandogasa]
    - Developed with AI assistance, declared in
      #link("https://github.com/slopfest/sandogasa/blob/main/AI-DECLARATION.md")[AI-DECLARATION.md]
  ],
  image("img/sandogasa.png", width: 100%),
)

== The tools\*

#table(
  columns: (auto, 1fr),
  stroke: none,
  inset: 5pt,
  [*cbspkg*], [Package and build for CentOS SIGs in CBS, the way fedpkg does for Fedora],
  [*cpu-sig-tracker*], [Track Proposed Updates across GitLab, Jira and CBS],
  [*hs-relmon*], [Release monitoring and housekeeping for Hyperscale packages],
  [*hs-intake*], [Compare a package's metadata between branches before taking it in],
  [*hs-meetings*], [Chair Hyperscale SIG meetings: day-of script, meetbot archive],
  [*ebranch*], [Get packages into EPEL, and release them there safely],
  [*fedora-cve-triage*], [Triage the CVE trackers in a maintainer's Bugzilla queue],
)

#text(size: 0.8em)[\* Those this talk covers. The other ten are out of scope
here; each has a README and a man page under `tools/<name>/` at
#link("https://github.com/slopfest/sandogasa")[github.com/slopfest/sandogasa].]

= A primer: two pipelines

== Fedora and EPEL

- *dist-git* (#link("https://src.fedoraproject.org")[src.fedoraproject.org]):
  one repo per package, one branch per release (`rawhide`, `f43`,
  `epel9`, `epel10`, …), worked with `fedpkg`
- *Koji* builds from dist-git; *Bodhi* turns builds into _updates_ that
  go to `testing`, collect karma, then go `stable`
- *Bugzilla* holds the bugs; an update names the bugs it fixes and
  closes them when it goes stable
- *EPEL* is the same machinery, building for RHEL and CentOS Stream; EPEL
  10 has a branch per RHEL minor (`epel10.2`, `epel10.3`, …)
- Pointers:
  #link("https://docs.fedoraproject.org/en-US/package-maintainers/")[Package Maintainer docs],
  #link("https://docs.fedoraproject.org/en-US/epel/")[EPEL docs]

== CentOS Stream and the SIGs

- *CentOS Stream* is where RHEL is developed in the open: a change is a
  GitLab merge request to `redhat/centos-stream/rpms/<pkg>`, and needs
  a RHEL Jira issue (`Resolves: RHEL-…`)
- *Special Interest Groups* ship packages beyond Stream, built in
  *CBS*, the Community Build Service
  (#link("https://cbs.centos.org")[cbs.centos.org], a Koji instance), from their own git repos, e.g.
  `CentOS/<sig>/rpms/<pkg>` on GitLab: CBS builds from a git URL, so
  the layout is the SIG's choice, not fixed as in Fedora's dist-git
- No Bodhi: a build moves through tags by hand,
  `<sig><release>-<collection>-candidate` → `-testing` → `-release`
- No Bugzilla: GitLab issues in the SIG's repos, and RHEL Jira
- Pointers:
  #link("https://sigs.centos.org/guide/")[SIG guide],
  #link("https://docs.centos.org/")[CentOS docs]

== Same shape, different names

#table(
  columns: (auto, 1fr, 1fr),
  inset: 6pt,
  align: left,
  [], [*Fedora / EPEL*], [*CentOS SIG*],
  [Source], [dist-git, `fedpkg`], [GitLab, `centpkg` or `cbspkg`],
  [Build], [Koji], [CBS (Koji)],
  [Gate], [Bodhi: testing → stable, karma], [Tags: -testing → -release, `cbspkg promote`],
  [Tracking], [Bugzilla], [GitLab issues, RHEL Jira],
  [Branching], [branch requests in Bugzilla], [a SIG branch in the SIG's repo],
)

== Two SIGs

- *Hyperscale*: for large deployments
  - newer packages and tooling than Stream ships
  - features RHEL leaves out, enabled again: Btrfs, building kpatches
  - branches `c9s-hs`, `c10s-hs`, built into the Hyperscale collections
- *Proposed Updates* (CPU): fixes proposed to CentOS Stream
  - built in CBS, pushed to the CentOS Stream mirrors
  - available while the merge request waits for Red Hat's maintainers
  - one branch per fix: `c10s-proposed-RHEL-303721`
- Both build in CBS with the same tag scheme, so cbspkg serves both

= CentOS Proposed Updates

== The life of a Proposed Update

+ A fix exists, upstream or in Fedora; a RHEL Jira issue covers it
  (`cpu-sig-tracker jira`)
+ A CentOS Stream merge request carries it (`cpu-sig-tracker mr`)
+ A tracking issue in the SIG's repo follows it (`cpu-sig-tracker file-issue`)
+ The SIG's repo gets the merge request as a branch (`cbspkg sig-repo`)
+ CBS builds it (`cbspkg build`), then it goes to testing and release
  (`cbspkg promote`)
+ Someone nudges a quiet merge request (`cpu-sig-tracker ping`, `status`)
+ CentOS Stream ships the fix: the SIG's build is untagged and the
  issue closed
  (`cpu-sig-tracker untag`, `retire`)

== cpu-sig-tracker

- `jira`: find the RHEL issue a merge request needs, or draft one
- `mr`: fork with `centpkg`, push, open the Stream merge request
- `file-issue`: the SIG's tracking issue for a merge request
- `list-issues`, `status`, `timeline`: where every update is, and where
  the time went
- `ping`: nudge merge requests that went quiet
- `sync-updates`: the SIG site's list of updates
- `untag`, `retire`: once CentOS Stream ships the fix

== cbspkg

- `sig-repo`: make sure the SIG has the package's repo (a pull mirror of
  Stream's), and carry merge requests into it as SIG branches
- `add-pkg`: list the package in the CBS tags its first build needs
- `srpm`, `mockbuild`, `verrel`: build locally the way CBS would, dist
  tag included
- `upload-sources`: the SIG lookaside
- `build`: build the checked-out branch in CBS
- `promote`: `-candidate` → `-testing` → `-release`
- Both comment on the tracking issue once the build gets there

== Example: rsync, CVE-2026-119741

- One fix, two Streams: merge requests
  #link("https://gitlab.com/redhat/centos-stream/rpms/rsync/-/merge_requests/90")[!90]
  (c10s) and
  #link("https://gitlab.com/redhat/centos-stream/rpms/rsync/-/merge_requests/91")[!91]
  (c9s)
- Each has its RHEL issue and a tracking issue in
  `CentOS/proposed_updates/rpms/rsync`, labelled `c10s` or `c9s` and
  `security`
- Each becomes a SIG branch named after release and RHEL issue:
  `c10s-proposed-RHEL-303721`

```sh
mr=https://gitlab.com/redhat/centos-stream/rpms/rsync/-/merge_requests/90
cpu-sig-tracker file-issue $mr
cbspkg sig-repo $mr
cbspkg build && cbspkg promote
```

== Example: git 2.52.1, 2026-11-13

- Prepared by Davide Cavalca: merge requests !32 (c10s) and !33 (c9s),
  tracking issues filed, SIG branches pushed
- Then CBS broke (`newRepo` tasks failing), and it was night in the US:
  the builds were handed over
- With no checkout yet, one command picked it up from the tracking
  issues:

```sh
cbspkg sig-repo --sig proposed-updates -C ~/src/centos/proposed_updates/rpms/git git
```

- It found both merge requests, cloned the SIG's repo, set up both
  branches, and left the checkout on the newest, `c10s-proposed-RHEL-306261`
- The c9s build stopped early: the package was never listed in its CBS
  tags, and `cbspkg build` offered to fix that
- c9s also needed a SIG-only commit (pinning Ruby for CBS's el9
  buildroot), which never goes to Stream: the c9s tracking issue lists
  it

== Tracking issues follow the build

```
git-2.52.1-1~proposed.el10 to -testing:
  cbs tag-build proposed_updates10s-packages-main-testing git-2.52.1-1~proposed.el10
Then comment on https://gitlab.com/CentOS/proposed_updates/rpms/git/-/work_items/1:
  > `git-2.52.1-1~proposed.el10` is ready for testing: tagged into
  > `proposed_updates10s-packages-main-testing`.
  >
  > Built from `a879194`, the merge request's head.
  and label it stage::testing
Promote? [Y/n]:
```

- The comment is part of what you confirm, and is posted only once the
  tag went through; `build` comments the same way
- It says what the build was made from, listing any SIG-only commits
- Hyperscale: the `rfe::new-version` issue is closed once every
  release has the version
- Fedora's equivalent is Bodhi commenting on the bug; here nothing did
  before November 2026

= Hyperscale

== hs-relmon: keeping up with upstream

- `check-latest`, `check-manifest`: is each package behind Fedora,
  upstream or Stream? File or update an `rfe::new-version` issue
- `check-stock`: has stock CentOS caught up? Then `retire` it: untag,
  drop from the manifest, archive the repo
- `review`: go through builds in `-testing`, promote or reject each
- `prune-tags`, `prune-manifest`, `prune-archived`: keep the tags tidy
- `dupe-subpkgs`, `file-conflicts`: two packages shipping the same thing
- `check-repos`: every package's GitLab repo set up the SIG's way

== hs-intake and hs-meetings

- *hs-intake*: before taking a package in or backporting it, compare
  its `BuildRequires`, `Requires` and `Provides` between branches;
  `safe-to-backport` sums it up
- *hs-meetings*: `script` prints the day-of zodbot script for chairing
  the SIG meeting, with follow-ups from last time and open tickets;
  `list` and `sync` keep the meetbot archive list current

= EPEL with ebranch

== Getting packages into EPEL

- `resolve`: the full dependency closure a package needs on the target
  branch; `check-crate` does it for a Rust crate
- `file-requests`: Bugzilla branch requests for everything missing,
  linked along the dependency graph; already-filed ones are adopted
- `escalate`: needinfo the maintainers of requests gone stale, per the
  EPEL policy; `close` once the package lands
- `check-wip`, `check-pkg-reviews`: what is still on its way in

== Releasing to EPEL safely

- `check-update`: would this update break anything already in the
  branch? Reverse dependencies, changed Provides, installability
- `--submit`: then file the Bodhi update, proposing the bugs it closes,
  CVE trackers included, and its type and severity
- Karma, from the same analysis; `update-bugs` revisits the bug list
- In November 2026, sandogasa 0.28.1 for EPEL 10.4 left off the 0.28.0 bug,
  because the 0.28.0 update for that release already had it

== Example: EternalTerminal in EPEL 10

- `et` needed `simpleini`, never branched for EPEL 10; one inventory,
  `[et, simpleini]`, with the edge `et → simpleini`
- Branch requests: et
  #link("https://bugzilla.redhat.com/show_bug.cgi?id=2368920")[rhbz\#2368920]
  (open since 2025-05-28), simpleini
  #link("https://bugzilla.redhat.com/show_bug.cgi?id=2543942")[rhbz\#2543942]
- `escalate` pinged simpleini's request, then took it to rel-eng
  (ticket 13584); et's ping was held until its dependency was branched
- Both were built together in one EPEL 10.4 update,
  FEDORA-EPEL-2026-f391e4e65a: submitted 2026-11-11, in testing as of
  2026-11-13

= Security: fedora-cve-triage

== A CVE queue, triaged

- A maintainer's queue fills with CVE trackers that need something
  other than a fix
- Each check recognizes one kind, with evidence, and proposes the
  resolution:
  - `bodhi-check`: already fixed by an update that went stable
  - `bundled-library`: in bundled code; fixed or not in what we ship
  - `cross-ecosystem`, `interpreter-fps`, `js-fps`: not this package's
    code at all
  - `fix-version`: filed against a branch it never shipped on
  - `rejected-cve`: the CVE was withdrawn
- `run` applies them all; each change is confirmed per bug, and you can
  take the bugs it closes

= Design notes

== `--explain`: show the commands

- Introduced with dbranch, for Debian packaging: where a tool's steps
  are commands a person could type, `--explain` narrates each step,
  shows the exact command, and pauses before running it
- `--dry-run` shows the same steps and runs nothing
- The point: learning the workflow, and checking the tool, from the
  same output; the printed commands are enough to do it by hand
- Not for API calls nobody would replay by hand: there, `-v` names
  each request, and anything written is previewed in full

== Writes are confirmed

- Changing CBS, GitLab, Bugzilla or Bodhi is confirmed at a terminal
- `--yes` does not reach places that cannot be edited afterwards
  (Red Hat Jira issues) without its own flag
- Cheap checks first: logins, tools, tokens, before the slow work
- Remote calls are batched: one request for many builds, bugs or merge
  requests, not one per item

= Where it stands

== Versions

As of 2026-11-13, in Fedora and EPEL:

- *0.28.0*: due to go stable in that night's compose
- *0.28.1*: due to go to testing (`updates-testing`, `epel-testing`)
  in the same compose, composes permitting
- *Only in git so far*, for the next release:
  - cbspkg: `sig-repo` taking over updates with no checkout, several
    merge requests at once; tracking-issue comments on `build` and
    `promote`; SIG-only commits; Hyperscale issues
  - cpu-sig-tracker: CVEs of updates fixing several; testing updates
    first on the SIG site; `file-issue` asks for the update's type
- Until then, from git:

```sh
cargo install --git https://github.com/slopfest/sandogasa cbspkg
```

== Future work

- Run hs-relmon and cpu-sig-tracker (`status --refresh`,
  `sync-updates`) on a schedule, so a change in status reaches the
  tracking issues and the SIG sites without anyone having to remember
- Comment on the tracking issue when a build is untagged, with the
  untag steps shared by both SIGs (\#144)
- Calendars and meetings: reading Fedora's fedocal (\#141, \#142)

== Getting started

```sh
sudo dnf install sandogasa          # Fedora and EPEL
cbspkg config                       # GitLab token, in the keyring
cpu-sig-tracker config              # GitLab and Jira tokens
ebranch config                      # Bugzilla API key
```

- Every tool has `--help` and a man page
- Bugs and ideas:
  #link("https://github.com/slopfest/sandogasa/issues")[github.com/slopfest/sandogasa/issues]
- These slides were drafted with Claude Code and reviewed by me; the
  tools' AI use is in AI-DECLARATION.md
