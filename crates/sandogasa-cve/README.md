# sandogasa-cve

Where a CVE is fixed, and whether a build has that fix.

- **`facts`** — `CveFacts` from an NVD response (fixed versions,
  vulnerable ranges, affected products, status, references);
  `Resolved` and `VersionSource` for a fix found elsewhere (GitHub's
  advisory database, a configured version, a range closed at the top);
  `is_fix(version, fixed, ranges)`, the judgment every user shares — at
  or past a fixed version *and* outside every range still marked
  vulnerable, so one series' fix does not vouch for another series'
  build; `product_matches_component` for NVD's product names against a
  Fedora component and its provides; `fix_facts`, the sources that
  need no one's confirmation asked in order of authority — NVD, GitHub's
  database by CVE, the repository advisories NVD's references name, a
  range closed at the top, the single release the references point at
  (`release_tag_candidates`) — for one `CveFix` a tool can judge a build
  against; `cve_id_in` and `is_security_tracker` for reading a Bugzilla
  summary.
- **`cache`** — `NvdCache`, NVD answers paced to its rate limit (5 per
  30 s bare, 50 with an API key), retried once after a refusal, kept
  on disk for a day under the calling tool's cache directory;
  `check_api_key`, what NVD says to a configured key.
- **`advisory`** — GitHub Security Advisories by CVE (`ghsa_records`,
  `ghsa_range`) and a fixed-version candidate read out of advisory
  prose (`fixed_version_candidates`) — a candidate a caller confirms,
  never a decision.
- **`bundled`** — judging a library copy a package bundles: the
  `bundled(<library>) = <version>` declaration (`bundled_version`,
  `declarations`, `copy_for_cve`), read from a build in Koji
  (`Copies::declared`), against the library's fixed versions
  (`against_fixes`) or by whether the copy's own release tag holds one
  of the fix commits OSV records (`Copies::git_verdict`). Answers that
  never change are kept under `$XDG_CACHE_HOME/sandogasa-cve/`, shared
  by every tool.
- **`severity`** — how severe a CVE is according to each source that
  says (`ratings`): Red Hat's security data, NVD's answer (with the
  CNA's and others' scores), and GitHub's advisory database, in that
  order, each as a `Rating` with a `Level` and the source's own words.
- **`version`** — `Nvr` parsing and `version_gte` on upstream version
  strings.

Used by `fedora-cve-triage` (closing CVE bugs a Bodhi update already
fixes), `ebranch` (which CVE bugs a security update closes) and
`cpu-sig-tracker` (a Proposed Update's Jira issue). DEVELOPMENT.md
records the design decisions.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
