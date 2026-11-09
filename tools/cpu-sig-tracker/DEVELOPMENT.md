# cpu-sig-tracker development notes

Design decisions and rules for future work. What the tool does is in
README.md; why it changed is in the workspace CHANGELOG.md.

## "Landed in stock" has one definition

`untag` removes the SIG's build because stock carries the change, and
`retire` closes the tracking issue as landed for the same reason. They
must never disagree: when `untag` judged by Jira alone while `retire`
also read stock's history, PackageKit's c10s update (fixed in stock by
Red Hat's own commit, its RHEL issue closed `Not a Bug`) could be
retired but not untagged, and the only way through was `--force`.

So the answer comes from one place, `landed::landed`, which both call:

- the SIG's MR merged;
- a commit on stock's branch naming the change (its RHEL key, a CVE,
  the MR's title or branch: `ping::evidence_tokens`);
- every RHEL issue fixed (see below).

`landed::tests::untag_and_retire_agree_on_what_landed` holds
`untag::precondition` to `retire`'s check for landed. New evidence
(such as matching the upstream commits stock's patches carry, #89)
goes into `landed`, never into one command.

## A Jira resolution is not a fix

An issue closed `Done` can have been closed for inactivity with
nothing fixed: blktrace's RHEL-123605 and RHEL-123606 went from New to
Closed with no fix version, and `retire` once took that as landed.
`jira::verdict_of` reads what a resolution means: fixed only with a fix
version, or as `Done-Errata`; unverified for a `Done` without one;
declined for `Won't Do`, `Not a Bug`, `Obsolete` and the like; a
duplicate judged by what it duplicates. Only fixed counts as landed;
declined and unverified suggest `retire`'s `declined` and `unanswered`
reasons.
