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
  range closed at the top — for one `CveFix` a tool can judge a build
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
- **`version`** — `Nvr` parsing and `version_gte` on upstream version
  strings.

Used by `fedora-cve-triage` (closing CVE bugs a Bodhi update already
fixes) and `ebranch` (which CVE bugs a security update closes).

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
