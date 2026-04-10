# TODO

## sandogasa-report

- Debug CVE/security bug reporting: the query may be too narrow or
  the keyword filter may not match Bugzilla's actual keyword values.
  Test with known CVE bugs and compare against manual Bugzilla search.

- Add date-range filtering to Koji CBS reporting (use `koji list-builds
  --after/--before` or tag history). Currently reports current tag
  contents, which misses packages that were untagged/replaced.
  Note: koji's `--before` is exclusive, add a day when converting
  from inclusive end dates.

## Koji tag cleanup tool

- Tool to detect and clean up stale builds in CentOS SIG Koji tags.
  Policy: only one build per package should be tagged for release
  unless there's a specific reason. The tool should list packages
  with multiple tagged builds and optionally untag the older ones.
