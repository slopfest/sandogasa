# fedora-cve-triage

A tool for triaging CVEs reported against Fedora components in Red Hat Bugzilla.

Fedora packages sometimes bundle third-party code (e.g. JavaScript/NodeJS
libraries for website assets) without shipping it. When CVEs are filed against those
libraries, tracking bugs get created against the Fedora packages even though
they're not affected. This tool helps identify and close those false positives.

## Usage

### Search for CVE bugs

```
$ fedora-cve-triage search -p "Fedora EPEL" -c cachelib
[2381749] CVE-2025-7339 cachelib: on-headers vulnerable to http response header manipulation [epel-9]
[2389816] CVE-2025-54881 cachelib: Mermaid cross site scripting [epel-9]
[2418496] CVE-2025-13466 cachelib: body-parser denial of service [epel-9]
[2421509] CVE-2025-12816 cachelib: node-forge: Interpretation conflict vulnerability allows bypassing cryptographic verifications [epel-9]
[2422467] CVE-2025-64718 cachelib: js-yaml prototype pollution in merge [epel-9]
[2422986] CVE-2025-66400 cachelib: mdast-util-to-hast: Markdown code elements can appear as regular page content [epel-9]
[2426469] CVE-2025-15284 cachelib: qs: Denial of Service via improper input validation in array parsing [epel-9]
...
```

### Detect JavaScript false positives

Create a TOML config file (see `configs/` for examples):

```toml
tracker_bug = "CVE-FalsePositive-Unshipped"
products = ["Fedora", "Fedora EPEL"]
components = ["cachelib", "fbthrift"]
statuses = ["NEW", "ASSIGNED"]
reason = "This project only ships JavaScript code as part of the website, the files are not shipped in the binary RPMs"
```

Then run:

```
$ fedora-cve-triage js-fps -f configs/js-fps-folly-stack.toml
Checking 3 CVE bugs for JavaScript false positives...
FP: bug 2418496 — CVE-2025-13466 cachelib: body-parser denial of service [epel-9]
FP: bug 2418504 — CVE-2025-13466 cachelib: body-parser denial of service [fedora-42]
FP: bug 2418510 — CVE-2025-13466 cachelib: body-parser denial of service [fedora-43]

3 likely false positive(s) found.
```

The tool searches Bugzilla for CVE bugs matching the configured products,
components, and statuses, then queries the [NVD API](https://nvd.nist.gov/)
to determine if each CVE is JavaScript-related using three strategies:

1. **CPE data** — checks the `target_sw` field for `node.js` (authoritative, when available)
2. **CNA source** — identifies CVEs filed by JavaScript-specific CNAs (e.g. OpenJS Foundation)
3. **Description keywords** — falls back to matching keywords like "javascript", "node.js", "npm" in the CVE description

NVD results are cached per CVE ID so duplicate bugs across products don't
cause redundant lookups.

### Close false positives

Add `--close-bugs` to close detected bugs as NOTABUG and mark them as
blocking the configured tracker bug. This requires a Bugzilla API key.

```
$ fedora-cve-triage js-fps -f configs/js-fps-folly-stack.toml --close-bugs
Checking 3 CVE bugs for JavaScript false positives...
FP: bug 2418496 — CVE-2025-13466 cachelib: body-parser denial of service [epel-9]
...

This will close 3 bug(s) as NOTABUG and mark them as blocking CVE-FalsePositive-Unshipped.
Proceed? [y/N] y
Closed 3 bug(s).
```

### Check for existing Bodhi fixes

The `bodhi-check` subcommand detects CVE bugs that are already fixed by a
Bodhi update. It queries NVD for fixed version information, then checks Bodhi
for updates containing builds that meet or exceed those versions.

Create a TOML config file:

```toml
tracker_bug = "CVE-AlreadyFixed"
products = ["Fedora", "Fedora EPEL"]
components = ["freerdp"]
# assignees = ["user@example.com"]  # optional: filter by assignee
statuses = ["NEW", "ASSIGNED"]
reason = "This bug is already fixed in a published Bodhi update."
lag_tolerance = 30
```

Then run:

```
$ fedora-cve-triage bodhi-check -f configs/bodhi-check-freerdp.toml
Checking 5 CVE bugs for existing Bodhi fixes...

Stable fixes (2):
  bug 2442801 — freerdp-3.23.0-1.fc42 (FEDORA-2026-abc123)
  bug 2442802 — freerdp-3.23.0-1.fc41 (FEDORA-2026-def456)

Testing fixes (1):
  bug 2442803 — freerdp-3.24.0-1.fc42 (FEDORA-2026-ghi789)
```

Add `--close-bugs` to close stable-fix bugs as ERRATA and mark late-filed
bugs as blocking the tracker. A bug is considered "late-filed" when it was
created after the Bodhi update's submission date plus `lag_tolerance` minutes —
meaning the fix was already available when the bug was filed. Late-filed bugs
that only have a testing fix are marked as blocking the tracker but are **not**
closed, since the update hasn't reached stable yet.

```
$ fedora-cve-triage bodhi-check -f configs/bodhi-check-freerdp.toml --close-bugs
...
This will close 2 bug(s) as ERRATA and mark 1 late-filed bug(s) as blocking CVE-AlreadyFixed.
Proceed? [y/N] y
Closed 2 bug(s) as ERRATA (freerdp-3.23.0-1.fc42)
Marked 1 bug(s) as blocking CVE-AlreadyFixed (late-filed)
```

Add `--edit-bodhi` to add bug references to testing updates via the `bodhi`
CLI (requires `bodhi-client` to be installed).

### Detect unshipped tool false positives

The `unshipped-tools` subcommand detects CVE bugs where the affected tool
(e.g. `xmllint`) is not shipped by the Fedora package that bundles the
upstream library (e.g. `pcem` bundles `libxml2` but doesn't ship `xmllint`).

```
$ fedora-cve-triage unshipped-tools -f configs/unshipped-tools.toml
```

### Configure Bugzilla API key

Required for `--close-bugs`. Create an API key at
https://bugzilla.redhat.com/userprefs.cgi?tab=apikey, then run:

```
$ fedora-cve-triage config
No config found at /home/user/.config/fedora-cve-triage/config.toml
Enter your Bugzilla email: user@example.com
Create an API key at https://bugzilla.redhat.com/userprefs.cgi?tab=apikey
Enter your Bugzilla API key:
```

### Verbose mode

Pass `-v` / `--verbose` to see progress details for rate-limited API queries:

```
$ fedora-cve-triage -v bodhi-check -f configs/bodhi-check.toml
```

## Library crates

This project is organized as a Cargo workspace. The underlying API clients
are published as reusable library crates under the **sandogasa** name
(菅笠, a Japanese straw hat often associated with "slum" or
post-apocalyptic robots):

- **sandogasa-bodhi** — Bodhi API client for Fedora update queries
- **sandogasa-bugzilla** — Bugzilla REST API client
- **sandogasa-nvd** — NVD (National Vulnerability Database) API client
- **sandogasa-distgit** — Fedora dist-git client and RPM spec file parser

## Building

```
cargo build --release
```

## License

This project is licensed under the [Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/).
