# Changelog

## v0.2.1

- Fall through to description heuristics when CPE has wildcard target_sw
- Hide API key input in config command

## v0.2.0

- Add bodhi-check subcommand to detect CVE bugs already fixed in Bodhi
- Add lag-tolerant tracker blocking for late-filed CVE bugs
- Add unit tests and enforce minimum coverage threshold

## v0.1.1

- Fix license text to MPL-2.0

## v0.1.0

- Initial release
- CLI with Bugzilla product/component/assignee/status filters
- js-fps subcommand to detect JavaScript/NodeJS false positives
- Three-strategy JS detection: CPE target_sw, CNA source, description keywords
- config command for Bugzilla API key setup
- Paginated Bugzilla search results
