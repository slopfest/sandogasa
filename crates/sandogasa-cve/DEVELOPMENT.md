# sandogasa-cve development notes

Design decisions and rules for future work. What the crate does is in
README.md; why it changed is in the workspace CHANGELOG.md.

## A CVE's severity comes from several sources, Red Hat's first

`severity::ratings` asks, in order:

1. **Red Hat's security data**
   (`access.redhat.com/hydra/rest/securitydata/cve/<id>.json`): its
   `threat_severity` and verified CVSS 3 score. For an RHEL issue this
   is the rating Red Hat itself weighs the issue by, so it comes first.
   It can disagree with everyone else: CVE-2026-2297 is `Low` (3.3) to
   Red Hat and medium (CVSS 4.0 5.7) to Python's CNA.
2. **NVD's answer**, through `CveResponse::cvss`: NVD's own score, or
   the CNA's and other parties', which NVD carries before it has
   analyzed a CVE.
3. **cve.org's record**, through `cveorg_ratings`: the CNA's score,
   then each other party's (an ADP container; CISA scores many CVEs
   their CNA did not, glibc's CVE-2025-4802 among them). The record is
   there before NVD copies it and when NVD does not answer. It comes
   after NVD so that the rating a caller uses first does not change
   where NVD has one, and a score NVD only passes on is not listed a
   second time: one with the same CVSS version and number as NVD's,
   when NVD's is not its own (`nvd@nist.gov`). NVD passes on other
   parties' scores as well as the CNA's: for CVE-2025-4802 its answer
   is CISA's 7.8, with CISA's id as the source. So for a CVE NVD has a
   record of, cve.org usually adds a line only when NVD scored it
   itself (xz's CVE-2024-3094: NVD's own 10 and the CNA's).
4. **GitHub's advisory database**, by CVE: a severity for what GitHub
   has reviewed, thin for C libraries but not empty (freetype's
   CVE-2025-27363 is there).

The four are asked together (`tokio::join!`), so adding a source does
not add its latency to the others'.

Red Hat's data is often missing exactly when it would matter most: a
CentOS Proposed Update is filed because Red Hat is behind, and it may
not have assessed the CVE yet. So no source is the only one, every
answer is kept, and a caller shows them side by side, so a
disagreement is visible rather than resolved silently.

### Parked: computing a score from OSV's CVSS vectors

OSV records carry `severity: [{"type": "CVSS_V3", "score":
"CVSS:3.1/AV:N/AC:L/…"}]`, which despite the field name is a vector
string, not a number. A fourth source would need the CVSS base score
computed from the vector: the CVSS 3.1 specification's base equations
(impact and exploitability subscores, scope changed or unchanged, the
round-up function) for `CVSS_V3`, and the far larger CVSS 4.0 lookup
tables for `CVSS_V4`. Not built, because the sources above have
covered every CVE checked so far. Build it, or take a crate for it
after checking it is packaged in Fedora and Debian, if a CVE turns up
that only OSV rates. The ecosystem databases OSV mirrors sometimes put
a plain word in `database_specific.severity`, which would be a cheaper
first step.

## cve.org is read beside NVD, through `Sources`

cve.org (`cveawg.mitre.org/api/cve/<id>`) is where a CVE is published;
NVD copies its records from there and adds its own analysis. The API
needs no key and its limit is far above what a run asks (25000 requests
a window on 2026-11-23, in its `ratelimit-*` headers), so there is no
client of its own and no pacing: `Sources::cveorg` fetches the record
like Red Hat's and OSV's, into the caller's disk cache, and
`cveorg::parse` reads it. An id that is only reserved answers 404, which
`Sources` gives as `None`, the same as no answer; a caller that must
tell the two apart (cpu-sig-tracker's disclosure check) makes the
request itself.

`Record::as_nvd` exists so that code written against `CveResponse` can
read a CVE NVD has no record of without a second code path. It is built
through JSON because NVD's models are `#[non_exhaustive]`. Two rules
keep it honest:

- It carries nothing NVD's analysis would add (no `configurations`, no
  `metrics`), so it reads exactly like NVD's own record of a CVE
  awaiting analysis. cve.org's scores are read from `Record::scores`,
  where they are attributed to who gave them, never through
  `CveResponse::cvss`, which would report them as NVD's.
- Its `vulnStatus` is `cveorg::STAND_IN_STATUS`, never `Rejected`. A
  rejection is asked of the record (`Record::is_rejected`), so that it
  is reported as cve.org's word.

### Parked: fixed versions from the CNA's `affected` data

A record's `affected` list is the CNA's own account of which versions
are vulnerable, available before NVD's CPE data. It is not read,
because CNAs write it in ways that cannot be judged alike. Six records
sampled on 2026-11-23 had five:

- the range inside the version string: `{"version": "< 2.32.4",
  "status": "affected"}` (GitHub's CNA, CVE-2024-47081);
- a range closed at the top with no fix: `version: "0.0.0",
  lessThanOrEqual: "2.13.0"` (CVE-2025-27363);
- a start and an exclusive end of `versionType: "custom"`:
  `version: "2.27", lessThan: "2.39"` (glibc, CVE-2025-4802);
- git commit hashes, plus a second entry of `unaffected` ranges with
  wildcards (`lessThanOrEqual: "5.4.*"`) (Linux, CVE-2024-53104);
- single affected versions for upstream beside RPM NEVRs per RHEL
  product (`0:3.4.1-2.el10`, `lessThan: "*"`, `status: "unaffected"`)
  (Red Hat, CVE-2024-12084).

`vendor` and `product` are free text (`n/a`, `Apache Log4j2`), so
matching one to a Fedora component needs more than
`product_matches_component` does today. Building this means a parser per
spelling that refuses what it does not recognize, the downstream entries
left out as `DOWNSTREAM_VENDORS` leaves them out of NVD's, and a place
in `fix_facts` after NVD's own data. What it decides closes bugs, so a
range read wrongly is worse than none.
