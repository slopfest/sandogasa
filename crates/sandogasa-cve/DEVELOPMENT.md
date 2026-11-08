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
   analysed a CVE.
3. **GitHub's advisory database**, by CVE: a severity for what GitHub
   has reviewed, thin for C libraries but not empty (freetype's
   CVE-2025-27363 is there).

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
tables for `CVSS_V4`. Not built, because the three sources above have
covered every CVE checked so far. Build it, or take a crate for it
after checking it is packaged in Fedora and Debian, if a CVE turns up
that only OSV rates. The ecosystem databases OSV mirrors sometimes put
a plain word in `database_specific.severity`, which would be a cheaper
first step.
