// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What each tool needs from each credential, in one table: the
//! services the tools keep a token or key for, where one is created, and,
//! per tool, the least it must be allowed to do and why; and the logins
//! the tools only check (Kerberos, the CBS certificate, the bodhi CLI's
//! session). A `config` prompt says from it what its tool needs and what
//! the other tools sharing the keyring entry need ([`token_help`]), and
//! `docs/tokens.md` is generated from it ([`markdown`]), so the two do
//! not disagree.
//!
//! The tools listed under each service are those in
//! [`crate::secret::USERS`], which a test keeps in step with the tools'
//! sources; another test keeps this table in step with that one.

/// A service the tools keep a token or key for, under one keyring label.
#[derive(Debug)]
pub struct Service {
    /// The keyring label the tools share ("sandogasa: GitLab token").
    pub label: &'static str,
    /// What to call the service ("GitLab").
    pub name: &'static str,
    /// What the secret is ("personal access token").
    pub what: &'static str,
    /// Where one is created; `{base}` stands for the instance's root URL.
    pub create: &'static str,
    /// Whether other instances of the service (GitHub Enterprise, a
    /// self-hosted sr.ht) keep their tokens elsewhere, so a fixed
    /// `create` page holds only for the service's own host; Jira's,
    /// NVD's and Fedora's pages serve every user.
    pub hosted: bool,
    /// What applies to every tool: how scopes work there, or that the
    /// secret is optional.
    pub note: &'static str,
    /// What one secret must grant to serve every tool using it, one
    /// scope or permission per item; empty when one tool does.
    pub all: &'static [&'static str],
    /// What each tool needs, sorted by tool.
    pub uses: &'static [Use],
}

/// What one tool needs from a service's secret.
#[derive(Debug)]
pub struct Use {
    pub tool: &'static str,
    /// The least it must grant ("read_api").
    pub needs: &'static str,
    /// What the tool does with it, which is why it needs that.
    pub why: &'static str,
}

/// A login a tool checks rather than keeps: another program's, or a
/// ticket or certificate.
#[derive(Debug)]
pub struct Login {
    pub name: &'static str,
    /// The tools that need it, sorted; empty for one only the release
    /// process uses.
    pub tools: &'static [&'static str],
    /// What they need it for.
    pub uses: &'static str,
    /// How to set it up.
    pub setup: &'static str,
}

/// Every service the tools keep a secret for, sorted by label.
pub const SERVICES: &[Service] = &[
    Service {
        label: "sandogasa: Bugzilla API key",
        name: "Bugzilla",
        what: "API key",
        create: "{base}/userprefs.cgi?tab=apikey",
        hosted: false,
        note: "A Bugzilla API key has no scopes: it can do whatever your account can.",
        all: &["an API key for each Bugzilla you use (Red Hat's; RPM Fusion's for ebranch)"],
        uses: &[
            Use {
                tool: "ebranch",
                needs: "an API key (Red Hat's; RPM Fusion's for its rebuild requests)",
                why: "files, updates and closes branch and rebuild requests, sets needinfo",
            },
            Use {
                tool: "fedora-cve-triage",
                needs: "an API key, to change bugs (reads work without one)",
                why: "closes, comments on and reassigns CVE bugs, sets needinfo",
            },
            Use {
                tool: "fedora-review-digest",
                needs: "an API key, for --post",
                why: "comments on review bugs and sets their fedora-review flag",
            },
            Use {
                tool: "poi-tracker",
                needs: "an API key, for the triage commands",
                why: "closes and comments on bugs, changes their priority",
            },
        ],
    },
    Service {
        label: "sandogasa: Forgejo token",
        name: "Forgejo",
        what: "access token",
        create: "{base}/user/settings/applications",
        hosted: false,
        note: "Each scope is a category, set to No access, Read, or Read and Write.",
        all: &[
            "issue: Read and Write",
            "organization: Read",
            "repository: Read",
            "user: Read",
        ],
        uses: &[
            Use {
                tool: "ebranch",
                needs: "issue Read and Write, organization Read",
                why: "escalate files releng tickets with the tracker's labels",
            },
            Use {
                tool: "fesco-chair",
                needs: "issue Read and Write, repository Read, user Read",
                why: "reads the agenda, comments on, labels and closes tickets",
            },
            Use {
                tool: "sandogasa-report",
                needs: "issue Read, repository Read, user Read",
                why: "searches your pull requests and issues, reads your activity feed",
            },
        ],
    },
    Service {
        label: "sandogasa: GitHub token",
        name: "GitHub",
        what: "personal access token",
        create: "https://github.com/settings/personal-access-tokens",
        hosted: true,
        note: "No tool writes to GitHub. A token with no scopes raises the rate limit \
               (60 to 5000 requests an hour); classic tokens are at \
               https://github.com/settings/tokens.",
        all: &[
            "no scopes",
            "repo (classic), to report on private repositories",
        ],
        uses: &[
            Use {
                tool: "ebranch",
                needs: "no scopes",
                why: "looks up security advisories; it has no prompt, and reads the entry \
                      another tool saved, or GITHUB_TOKEN",
            },
            Use {
                tool: "fedora-cve-triage",
                needs: "no scopes",
                why: "looks up security advisories and repository languages",
            },
            Use {
                tool: "sandogasa-report",
                needs: "no scopes; repo (classic) for --private",
                why: "reads your pull requests, issues and events; --private counts \
                      private repositories",
            },
        ],
    },
    Service {
        label: "sandogasa: GitLab token",
        name: "GitLab",
        what: "personal access token",
        create: "{base}/-/user_settings/personal_access_tokens",
        hosted: false,
        note: "",
        all: &["api, in a legacy (not fine-grained) token"],
        uses: &[
            Use {
                tool: "cbspkg",
                needs: "api; Maintainer in the SIG's group for sig-repo",
                why: "creates SIG repositories, comments on and labels tracking issues",
            },
            Use {
                tool: "cpu-sig-tracker",
                needs: "api, in a legacy (not fine-grained) token",
                why: "files, labels and comments on tracking issues, opens merge requests",
            },
            Use {
                tool: "hs-relmon",
                needs: "api",
                why: "files, labels and comments on issues, archives retired projects",
            },
            Use {
                tool: "sandogasa-report",
                needs: "read_api",
                why: "reads your events, merge requests, issues, tags and releases",
            },
        ],
    },
    Service {
        label: "sandogasa: Jira API token",
        name: "Jira",
        what: "Atlassian API token",
        create: "https://id.atlassian.com/manage-profile/security/api-tokens",
        hosted: false,
        note: "Entered with your Atlassian email; public issues read without one.",
        all: &[],
        uses: &[Use {
            tool: "cpu-sig-tracker",
            needs: "read:jira-user, read:jira-work; write:jira-work to file issues",
            why: "reads RHEL issues, and files them with jira --file",
        }],
    },
    Service {
        label: "sandogasa: NVD API key",
        name: "NVD",
        what: "API key",
        create: "https://nvd.nist.gov/developers/request-an-api-key",
        hosted: false,
        note: "Optional: a key raises the rate limit from 5 to 50 requests per 30 s.",
        all: &["an API key"],
        uses: &[
            Use {
                tool: "ebranch",
                needs: "an API key",
                why: "check-update judges CVE bugs by their NVD records",
            },
            Use {
                tool: "fedora-cve-triage",
                needs: "an API key",
                why: "reads NVD records for the CVEs it triages",
            },
        ],
    },
    Service {
        label: "sandogasa: Pagure token",
        name: "Fedora dist-git (Pagure)",
        what: "API token",
        create: "https://src.fedoraproject.org/settings/token/new",
        hosted: false,
        note: "",
        all: &["the \"Modify an existing project\" ACL"],
        uses: &[
            Use {
                tool: "poi-tracker",
                needs: "the \"Modify an existing project\" ACL",
                why: "adopt takes orphaned packages; act orphans or gives them away",
            },
            Use {
                tool: "sandogasa-pkg-acl",
                needs: "the \"Modify an existing project\" ACL",
                why: "sets and removes access, gives and takes packages",
            },
        ],
    },
    Service {
        label: "sandogasa: SourceHut token",
        name: "Sourcehut",
        what: "personal access token",
        create: "https://meta.sr.ht/oauth2/personal-token",
        hosted: true,
        note: "Tick \"read-only access token\": every grant is then read-only.",
        all: &[],
        uses: &[Use {
            tool: "sandogasa-report",
            needs: "read-only; all services, or git.sr.ht, lists.sr.ht, todo.sr.ht and meta.sr.ht",
            why: "reads your repositories, patchsets, tickets and profile email",
        }],
    },
];

/// The logins the tools check but do not keep, sorted by name.
pub const LOGINS: &[Login] = &[
    Login {
        name: "bodhi CLI session",
        tools: &["ebranch", "fedora-cve-triage"],
        uses: "ebranch check-update --give-karma and --submit, update-bugs: \
               karma, new updates, bug lists; fedora-cve-triage --edit-bodhi",
        setup: "Run any authenticated bodhi command once \
                (`bodhi overrides query --mine`); its login is cached in \
                ~/.config/bodhi/client.json.",
    },
    Login {
        name: "CentOS Build System certificate",
        tools: &["cbspkg", "cpu-sig-tracker", "hs-relmon"],
        uses: "cbspkg build, promote, untag, upload-sources, add-pkg; \
               cpu-sig-tracker untag; hs-relmon's untagging commands",
        setup: "`kinit <you>@FEDORAPROJECT.ORG`, then `centos-cert -u <you>`, which \
                writes ~/.centos.cert; `centos-cert -v` checks it. cbspkg offers to \
                run it when CBS refuses the login.",
    },
    Login {
        name: "centpkg token",
        tools: &["cpu-sig-tracker"],
        uses: "cpu-sig-tracker mr (`centpkg fork`)",
        setup: "An API token in ~/.config/rpkg/centpkg.conf, as centpkg documents.",
    },
    Login {
        name: "copr-cli",
        tools: &["ebranch"],
        uses: "ebranch's Copr pruning (`copr-cli delete-package`)",
        setup: "copr-cli's own API token, in ~/.config/copr.",
    },
    Login {
        name: "crates.io",
        tools: &[],
        uses: "releasing sandogasa (`cargo ws publish`)",
        setup: "`cargo login`, which keeps the token in the desktop keyring when \
                ~/.cargo/config.toml sets \
                `global-credential-providers = [\"cargo:libsecret\"]`.",
    },
    Login {
        name: "Debusine",
        tools: &["dbranch"],
        uses: "dbranch --debusine uploads",
        setup: "Install debusine-client and run `debusine setup`.",
    },
    Login {
        name: "Fedora Kerberos ticket",
        tools: &[
            "fesco-chair",
            "sandogasa-hattrack",
            "sandogasa-report",
            "timebook",
        ],
        uses: "timebook cancel and edit, sandogasa-hattrack away (Fedora Calendar); \
               fesco-chair votes, sandogasa-hattrack and sandogasa-report (FASJSON reads)",
        setup: "`fkinit -u <you>` or `kinit <you>@FEDORAPROJECT.ORG`, with your FAS \
                username in ~/.fedora.upn.",
    },
    Login {
        name: "fedpkg",
        tools: &["ebranch"],
        uses: "ebranch file-requests (`fedpkg request-branch`)",
        setup: "fedpkg's own Pagure token (`fedpkg set-distgit-token`).",
    },
    Login {
        name: "glab",
        tools: &["dbranch"],
        uses: "dbranch on Salsa: pushes, CI, new projects",
        setup: "`glab auth login --hostname salsa.debian.org`; a gitlab.com login does \
                not cover Salsa.",
    },
];

/// The tools that need no credentials at all, sorted.
pub const NO_CREDENTIALS: &[&str] = &[
    "hs-intake",
    "hs-meetings",
    "koji-diff",
    "koji-lag",
    "sandogasa-pkg-health",
    "specport",
];

/// The service kept under `label`.
pub fn service(label: &str) -> Option<&'static Service> {
    SERVICES.iter().find(|s| s.label == label)
}

/// What a `config` prompt says before asking `tool` for the secret
/// kept under `label` on the instance at `base`: where to create one,
/// and what to grant, as a list: what this tool needs, then, since the
/// keyring entry is shared, what each other tool using it needs, and
/// what one secret needs for all of them, so someone who runs only this
/// tool can grant less. The reasons are on the docs page. One line per
/// item, each indented by two spaces, ending in a newline; empty for a
/// label not in the table.
pub fn token_help(label: &str, tool: &str, base: &str) -> String {
    let Some(service) = service(label) else {
        return String::new();
    };
    let mut out = String::new();
    // A page on the service's own host says nothing about another
    // instance of it (GitHub Enterprise, a self-hosted sr.ht).
    let create = match !service.hosted
        || service.create.contains("{base}")
        || same_site(base, service.create)
    {
        true => format!(
            "Create {} {} {} at {}",
            article(service.name).to_lowercase(),
            service.name,
            service.what,
            creation_url(service.create, base)
        ),
        false => format!(
            "Create {} {} {} in your settings on {}",
            article(service.name).to_lowercase(),
            service.name,
            service.what,
            base.trim_end_matches('/')
        ),
    };
    wrap(&mut out, &create, "  ", "    ");
    if !service.note.is_empty() {
        wrap(&mut out, service.note, "  ", "  ");
    }
    // This tool first, then the others sharing the keyring entry, then
    // what one token needs for all of them.
    out.push_str("  What to grant:\n");
    if let Some(own) = service.uses.iter().find(|u| u.tool == tool) {
        wrap(
            &mut out,
            &format!("- {tool} (this tool): {}", own.needs),
            "  ",
            "    ",
        );
    }
    for u in service.uses.iter().filter(|u| u.tool != tool) {
        wrap(
            &mut out,
            &format!("- {}: {}", u.tool, u.needs),
            "  ",
            "    ",
        );
    }
    if !service.all.is_empty() && service.uses.len() > 1 {
        out.push_str("  For all of them:\n");
        for item in service.all {
            wrap(&mut out, item, "    ", "      ");
        }
    }
    out
}

/// The credentials page, `docs/tokens.md`, as Markdown.
pub fn markdown() -> String {
    let mut out = String::from(
        "<!-- Generated from crates/sandogasa-config/src/credentials.rs by \
         `make tokens`; edit the table there, not this file. -->\n\n\
         # Tokens and logins\n\n\
         What each tool needs from each service, and where to set it up. Each \
         tool's `config` command says the same for its own tokens.\n\n\
         A token or key is kept in the desktop keyring (GNOME Keyring, or \
         KWallet with its Secret Service support), one entry per service and \
         host, labeled \"sandogasa: <kind of token>\", and shared by every tool \
         that uses that service. Without a keyring it goes in the tool's own \
         config file. A token one tool needs more from than another must grant \
         the most any tool you run needs; if you run only one, grant what that \
         one needs.\n\n\
         To replace one token, with one granting more or a new one: \
         `<tool> config --token <host>` asks for it again, saying what to \
         grant.\n\n\
         From a terminal:\n\n\
         - `secret-tool lookup application sandogasa service <host>` shows an \
         entry\n\
         - `secret-tool clear application sandogasa service <host>` removes it, \
         for every tool that uses it; to stop one tool using it, clear its token \
         in that tool's `config` instead (enter `-`)\n\n\
         ## By tool\n\n\
         What each tool needs; the sections below say where to create each \
         token and what the other tools sharing it need.\n\n\
         | Tool | Tokens and API keys | Logins |\n| --- | --- | --- |\n",
    );
    for tool in tools() {
        let tokens: Vec<String> = SERVICES
            .iter()
            .flat_map(|s| s.uses.iter().map(move |u| (s, u)))
            .filter(|(_, u)| u.tool == tool)
            .map(|(s, u)| format!("[{}](#{}): {}", s.name, anchor(s.name), u.needs))
            .collect();
        let logins: Vec<&str> = LOGINS
            .iter()
            .filter(|l| l.tools.contains(&tool))
            .map(|l| l.name)
            .collect();
        let cell = |items: Vec<String>| match items.is_empty() {
            true => "none".to_string(),
            false => items.join("<br>"),
        };
        out.push_str(&format!(
            "| {tool} | {} | {} |\n",
            cell(tokens),
            cell(logins.iter().map(|l| l.to_string()).collect())
        ));
    }
    out.push_str("\n## Tokens and API keys\n");
    for s in SERVICES {
        out.push_str(&format!("\n### {}\n\n", s.name));
        // A URL with the instance left to fill in is no link.
        let url = match s.create.contains("{base}") {
            true => format!("`{}`", creation_url(s.create, "https://<instance>")),
            false => format!("<{}>", s.create),
        };
        out.push_str(&format!(
            "{} {}, created at {url}.",
            article(s.what),
            s.what,
        ));
        if !s.note.is_empty() {
            out.push_str(&format!(" {}", s.note));
        }
        out.push_str("\n\n| Tool | Needs | For |\n| --- | --- | --- |\n");
        for u in s.uses {
            out.push_str(&format!("| {} | {} | {} |\n", u.tool, u.needs, u.why));
        }
        if !s.all.is_empty() {
            out.push_str("\nFor all of these tools:\n\n");
            for item in s.all {
                out.push_str(&format!("- {item}\n"));
            }
        }
    }
    out.push_str(
        "\n## Logins the tools check\n\n\
         These are not kept by sandogasa: each is another program's login, or a \
         ticket or certificate. The tools check them before working and say how \
         to set them up when they are missing.\n\n\
         | Login | Used by | Setup |\n| --- | --- | --- |\n",
    );
    for l in LOGINS {
        out.push_str(&format!("| {} | {} | {} |\n", l.name, l.uses, l.setup));
    }
    out
}

/// Append `text` to `out` wrapped at 78 columns, its first line after
/// `first`, the others after `rest`, each ending in a newline.
fn wrap(out: &mut String, text: &str, first: &str, rest: &str) {
    const WIDTH: usize = 78;
    let mut line = first.to_string();
    let mut empty = true;
    for word in text.split_whitespace() {
        let width = line.chars().count() + 1 + word.chars().count();
        if !empty && width > WIDTH {
            out.push_str(&line);
            out.push('\n');
            line = rest.to_string();
            empty = true;
        }
        if !empty {
            line.push(' ');
        }
        line.push_str(word);
        empty = false;
    }
    out.push_str(&line);
    out.push('\n');
}

/// The lowercased host of `url`, with or without a scheme.
fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or("").to_lowercase()
}

/// Whether `base` is the site `page` is on, or names none: one host is
/// the other or under it (api.github.com and github.com, sr.ht and
/// meta.sr.ht).
fn same_site(base: &str, page: &str) -> bool {
    let (a, b) = (host(base), host(page));
    a.is_empty() || a == b || a.ends_with(&format!(".{b}")) || b.ends_with(&format!(".{a}"))
}

/// Every tool the page covers, sorted: those keeping a secret or
/// checking a login, and those needing none.
fn tools() -> Vec<&'static str> {
    let mut all: Vec<&str> = SERVICES
        .iter()
        .flat_map(|s| s.uses.iter().map(|u| u.tool))
        .chain(LOGINS.iter().flat_map(|l| l.tools.iter().copied()))
        .chain(NO_CREDENTIALS.iter().copied())
        .collect();
    all.sort_unstable();
    all.dedup();
    all
}

/// The anchor GitHub gives a heading: lowercased, spaces as `-`, and
/// punctuation other than `-` dropped.
fn anchor(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' => Some(c),
            _ => None,
        })
        .collect()
}

/// "A" or "An" before `word`.
fn article(word: &str) -> &'static str {
    let first = word.split_whitespace().next().unwrap_or("");
    // An acronym is read letter by letter: "an NVD", "an API key".
    let acronym = first.len() > 1 && first.chars().all(|c| c.is_ascii_uppercase());
    let vowel_sound = match first.chars().next() {
        Some(c) if acronym => "AEFHILMNORSX".contains(c),
        Some(c) => "aeiouAEIOU".contains(c),
        None => false,
    };
    match vowel_sound {
        true => "An",
        false => "A",
    }
}

/// `create` with the instance filled in.
fn creation_url(create: &str, base: &str) -> String {
    create.replace("{base}", base.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tools under each label are those `secret::USERS` lists,
    /// which a test keeps in step with the tools' sources.
    #[test]
    fn the_table_lists_the_tools_that_keep_each_secret() {
        let ours: Vec<(&str, Vec<&str>)> = SERVICES
            .iter()
            .map(|s| (s.label, s.uses.iter().map(|u| u.tool).collect()))
            .collect();
        let theirs: Vec<(&str, Vec<&str>)> = crate::secret::USERS
            .iter()
            .map(|(label, tools)| (*label, tools.to_vec()))
            .collect();
        assert_eq!(ours, theirs);
    }

    #[test]
    fn a_prompt_says_what_this_tool_and_the_others_need() {
        let help = token_help(
            "sandogasa: GitLab token",
            "sandogasa-report",
            "https://gitlab.com/",
        );
        assert!(
            help.starts_with(
                "  Create a GitLab personal access token at\n    \
                 https://gitlab.com/-/user_settings/personal_access_tokens\n"
            ),
            "{help}"
        );
        // This tool first, then the others, then what all of them need.
        assert!(
            help.ends_with(
                "  What to grant:\n\
                 \x20 - sandogasa-report (this tool): read_api\n\
                 \x20 - cbspkg: api; Maintainer in the SIG's group for sig-repo\n\
                 \x20 - cpu-sig-tracker: api, in a legacy (not fine-grained) token\n\
                 \x20 - hs-relmon: api\n\
                 \x20 For all of them:\n\
                 \x20   api, in a legacy (not fine-grained) token\n"
            ),
            "{help}"
        );
        assert!(help.lines().all(|l| l.chars().count() <= 78), "{help}");
        // One user only: no others to list.
        let jira = token_help(
            "sandogasa: Jira API token",
            "cpu-sig-tracker",
            "https://redhat.atlassian.net",
        );
        assert!(!jira.contains("For all of them"), "{jira}");
        // Atlassian's page serves every site, whichever one is asked about.
        assert!(
            jira.starts_with(
                "  Create a Jira Atlassian API token at\n    \
                 https://id.atlassian.com/manage-profile/security/api-tokens\n"
            ),
            "{jira}"
        );
        assert_eq!(token_help("sandogasa: nothing", "x", "https://x"), "");
    }

    /// A secret several tools share says what one token needs for all
    /// of them; one only one tool keeps does not.
    #[test]
    fn a_shared_secret_says_what_all_its_tools_need() {
        for s in SERVICES {
            assert_eq!(s.uses.len() > 1, !s.all.is_empty(), "{}", s.label);
        }
        let help = token_help(
            "sandogasa: Forgejo token",
            "sandogasa-report",
            "https://forge.example",
        );
        assert!(
            help.ends_with(
                "  For all of them:\n    issue: Read and Write\n    organization: Read\n    \
                 repository: Read\n    user: Read\n"
            ),
            "{help}"
        );
    }

    /// `docs/tokens.md` is this table, when the crate is built in a
    /// checkout; `make tokens` writes it.
    #[test]
    fn the_docs_page_matches_the_table() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        // Only the workspace has the page: a crate's tarball has
        // neither it nor the workspace around it, wherever it is
        // unpacked (debcargo's build/ sits in another git checkout).
        if !root
            .join("crates/sandogasa-config/src/credentials.rs")
            .exists()
        {
            return;
        }
        let text = std::fs::read_to_string(root.join("docs/tokens.md"))
            .expect("docs/tokens.md is missing: run `make tokens`");
        assert!(
            text == markdown(),
            "docs/tokens.md is out of date: run `make tokens`"
        );
    }

    /// Every tool in the workspace is on the page, under a service,
    /// a login, or as needing nothing.
    #[test]
    fn every_tool_is_on_the_page() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let Ok(dirs) = std::fs::read_dir(root.join("tools")) else {
            return;
        };
        if !root
            .join("crates/sandogasa-config/src/credentials.rs")
            .exists()
        {
            return;
        }
        let mut found: Vec<String> = dirs
            .filter_map(|d| d.ok())
            .filter(|d| d.path().join("Cargo.toml").exists())
            .map(|d| d.file_name().to_string_lossy().into_owned())
            .collect();
        found.sort();
        assert_eq!(found, tools(), "add the new tool to credentials.rs");
    }

    #[test]
    fn the_page_has_a_table_per_service_and_one_of_logins() {
        let md = markdown();
        for s in SERVICES {
            assert!(md.contains(&format!("\n### {}\n", s.name)), "{}", s.name);
        }
        assert!(
            md.contains("created at `https://<instance>/-/user_settings/personal_access_tokens`.")
        );
        assert!(md.contains("created at <https://meta.sr.ht/oauth2/personal-token>."));
        assert!(md.contains("| crates.io | releasing sandogasa"));
        assert!(md.contains(
            "| sandogasa-report | [Forgejo](#forgejo): issue Read, repository Read, user Read<br>"
        ), "{md}");
        assert!(md.contains("| specport | none | none |"), "{md}");
        assert!(
            md.contains("[Fedora dist-git (Pagure)](#fedora-dist-git-pagure)"),
            "{md}"
        );
        for l in LOGINS {
            assert!(l.tools.windows(2).all(|w| w[0] < w[1]), "{}", l.name);
        }
        assert_eq!(article("API key"), "An");
        assert_eq!(article("NVD"), "An");
        assert_eq!(article("GitHub"), "A");
        assert_eq!(article("Bugzilla"), "A");
        assert!(same_site(
            "https://api.github.com",
            "https://github.com/settings"
        ));
        assert!(same_site(
            "sr.ht",
            "https://meta.sr.ht/oauth2/personal-token"
        ));
        assert!(same_site("", "https://github.com/settings"));
        assert!(!same_site(
            "https://github.example.com",
            "https://github.com/settings"
        ));
        let ghe = token_help(
            "sandogasa: GitHub token",
            "sandogasa-report",
            "https://ghe.example/",
        );
        assert!(
            ghe.starts_with(
                "  Create a GitHub personal access token in your settings on\n    \
                 https://ghe.example\n"
            ),
            "{ghe}"
        );
        // A word longer than a line stays whole, on a line of its own.
        let mut out = String::new();
        wrap(&mut out, &format!("a {} b", "x".repeat(90)), "  ", "    ");
        assert_eq!(out, format!("  a\n    {}\n    b\n", "x".repeat(90)));
        assert_eq!(article("personal access token"), "A");
    }
}
