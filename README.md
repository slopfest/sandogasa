# fedora-cve-triage

A tool for triaging CVEs reported against Fedora components in Red Hat Bugzilla.

Fedora packages sometimes bundle third-party code (e.g. NodeJS libraries for
website assets) without shipping it. When CVEs are filed against those
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

### Detect NodeJS false positives

Create a TOML config file (see `configs/` for examples):

```toml
tracker_bug = "CVE-FalsePositive-Unshipped"
products = ["Fedora", "Fedora EPEL"]
components = ["cachelib"]
statuses = ["NEW", "ASSIGNED"]
```

Then run:

```
$ fedora-cve-triage nodejs-fps -f configs/nodejs-fps-cachelib.toml
Found 18 CVE bugs to check
FP: bug 2381749 (Fedora EPEL / cachelib) — CVE-2025-7339 targets node.js
FP: bug 2381757 (Fedora / cachelib) — CVE-2025-7339 targets node.js
FP: bug 2418525 (Fedora / cachelib) — CVE-2025-66031 targets node.js
FP: bug 2421509 (Fedora EPEL / cachelib) — CVE-2025-12816 targets node.js
FP: bug 2421512 (Fedora / cachelib) — CVE-2025-12816 targets node.js
FP: bug 2422467 (Fedora EPEL / cachelib) — CVE-2025-64718 targets node.js
FP: bug 2422479 (Fedora / cachelib) — CVE-2025-64718 targets node.js
FP: bug 2422986 (Fedora EPEL / cachelib) — CVE-2025-66400 targets node.js
FP: bug 2422987 (Fedora / cachelib) — CVE-2025-66400 targets node.js
FP: bug 2426469 (Fedora EPEL / cachelib) — CVE-2025-15284 targets node.js
...

13 likely false positive(s) found.
```

The tool searches Bugzilla for CVE bugs matching the configured products,
components, and statuses, then queries the [NVD API](https://nvd.nist.gov/)
to check if each CVE targets `node.js` in its CPE data. When CPE
configurations are not yet available (e.g. CVEs still "Awaiting Analysis"),
it falls back to keyword matching on the CVE description. NVD results are
cached per CVE ID so duplicate bugs across products don't cause redundant
lookups.

### Close false positives

Add `--close-bugs` to close detected bugs as NOTABUG and mark them as
blocking the configured tracker bug. This requires a Bugzilla API key.

```
$ fedora-cve-triage nodejs-fps -f configs/nodejs-fps-cachelib.toml --close-bugs
Found 18 CVE bugs to check
FP: bug 2381749 (Fedora EPEL / cachelib) — CVE-2025-7339 targets node.js
...

13 likely false positive(s) found.

This will close 13 bug(s) as NOTABUG and mark them as blocking CVE-FalsePositive-Unshipped.
Proceed? [y/N]
```

### Configure Bugzilla API key

Required for `--close-bugs`. Create an API key at
https://bugzilla.redhat.com/userprefs.cgi?tab=apikey, then run:

```
$ fedora-cve-triage config
No config found at /home/user/.config/fedora-cve-triage/config.toml
Create an API key at https://bugzilla.redhat.com/userprefs.cgi?tab=apikey
Enter your Bugzilla API key:
```

## Building

```
cargo build --release
```

## License

This project is licensed under the [Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/).
