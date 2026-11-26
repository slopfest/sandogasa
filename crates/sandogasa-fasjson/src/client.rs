// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::process::Command;

use crate::models::{FasGroup, FasUser, FasjsonPage, FasjsonResponse, PageInfo};

const FASJSON_BASE: &str = "https://fasjson.fedoraproject.org";

/// What this crate calls itself to a server, matching the string the
/// reqwest-based crates here send through `sandogasa_cli::http`.
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

/// How this crate invokes curl.
///
/// Separate from the call so the arguments can be asserted: the
/// user-agent in particular is invisible when it works and costs a
/// tarpitted request when it is missing, which is not a thing to leave to
/// inspection.
///
/// `--` so the URL can never be read as a curl option. `--max-time` so a
/// hung connection cannot block forever, at the same 120s bound the
/// reqwest-based sibling crates use. `--user-agent` because Fedora's
/// infrastructure tarpits requests that arrive without one, and inheriting
/// curl's default would leave this crate the only client here that a
/// server log cannot identify.
///
/// Each of `headers` is sent with `-H`, e.g. FASJSON's `X-Fields` field
/// mask.
fn curl_args<'a>(url: &'a str, headers: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "--negotiate",
        "-u",
        ":",
        "-sf",
        "--max-time",
        "120",
        "--user-agent",
        USER_AGENT,
    ];
    for header in headers {
        args.extend(["-H", header]);
    }
    args.extend(["--", url]);
    args
}

/// The fields of a group a list of groups is asked for: FASJSON lists a
/// user's groups by name alone unless told which fields to include.
const GROUP_FIELDS: &str = "X-Fields: groupname,description,mailing_list";

/// `name` as one segment of a URL path: everything but unreserved
/// characters percent-encoded, so a name with a space or a slash cannot
/// reach another endpoint.
fn path_segment(name: &str) -> String {
    name.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Every item of a paged FASJSON list, asking `fetch` for page 1, 2, …
/// until the paging block says there are no more. `check` sees the first
/// page's paging block and can refuse the list before the rest is
/// fetched.
fn collect_pages<T>(
    mut fetch: impl FnMut(u32) -> Result<FasjsonPage<T>, Box<dyn std::error::Error>>,
    check: impl Fn(&PageInfo) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<Vec<T>, Box<dyn std::error::Error>> {
    let mut items = Vec::new();
    let mut page_number = 1;
    loop {
        let page = fetch(page_number)?;
        if page_number == 1
            && let Some(p) = &page.page
        {
            check(p)?;
        }
        items.extend(page.result);
        match page.page {
            Some(p) if p.page_number < p.total_pages => page_number = p.page_number + 1,
            _ => break,
        }
    }
    Ok(items)
}

pub struct FasjsonClient {
    base_url: String,
    /// The curl to run, with any arguments it starts with: `curl` from
    /// `PATH`, or a test's stand-in.
    curl: Vec<String>,
}

impl Default for FasjsonClient {
    fn default() -> Self {
        Self::new()
    }
}

impl FasjsonClient {
    pub fn new() -> Self {
        Self::with_base_url(FASJSON_BASE)
    }

    pub fn with_base_url(base_url: &str) -> Self {
        sandogasa_cli::install_crypto_provider();

        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            curl: vec!["curl".to_string()],
        }
    }

    /// Run `sh <script>` instead of curl: a test's script that answers
    /// like FASJSON. Read by sh rather than executed, so a test thread
    /// starting a process while another writes its script cannot make
    /// it "Text file busy".
    #[cfg(test)]
    fn with_curl(mut self, script: &str) -> Self {
        self.curl = vec!["sh".to_string(), script.to_string()];
        self
    }

    /// Fetch a user profile from FASJSON using Kerberos negotiate auth.
    ///
    /// Shells out to `curl --negotiate` since FASJSON requires GSSAPI
    /// authentication and there is no pure-Rust GSSAPI implementation
    /// that avoids a build-time dependency on system krb5 libraries.
    pub fn user(&self, username: &str) -> Result<FasUser, Box<dyn std::error::Error>> {
        let url = format!("{}/v1/users/{}/", self.base_url, path_segment(username));
        let resp: FasjsonResponse<FasUser> = self.get_json(&url)?;
        Ok(resp.result)
    }

    /// Every member of the FAS group `group`, walking FASJSON's pages.
    /// Membership is what FAS says today; sponsors are members too.
    /// With `max`, a group FAS counts as larger is refused after the
    /// first page, before the rest is fetched — the caller's cap on
    /// how many people it means to look up.
    pub fn group_members(
        &self,
        group: &str,
        max: Option<usize>,
    ) -> Result<Vec<FasUser>, Box<dyn std::error::Error>> {
        collect_pages(
            |page_number| {
                self.get_json(&format!(
                    "{}/v1/groups/{}/members/?page_size=100&page_number={page_number}",
                    self.base_url,
                    path_segment(group)
                ))
            },
            |p| match max {
                Some(max) if p.total_results as usize > max => Err(format!(
                    "{group} has {} members and the cap is {max}; raise --max-members if you \
                     mean to look up every one of them",
                    p.total_results
                )
                .into()),
                _ => Ok(()),
            },
        )
    }

    /// The FAS group `name`, with its mailing list.
    pub fn group(&self, name: &str) -> Result<FasGroup, Box<dyn std::error::Error>> {
        let url = format!("{}/v1/groups/{}/", self.base_url, path_segment(name));
        let resp: FasjsonResponse<FasGroup> = self.get_json(&url)?;
        Ok(resp.result)
    }

    /// Every group the user `username` is a member of (sponsors
    /// included), with each group's mailing list, walking FASJSON's
    /// pages.
    pub fn user_groups(&self, username: &str) -> Result<Vec<FasGroup>, Box<dyn std::error::Error>> {
        collect_pages(
            |page_number| {
                self.get_json_with(
                    &format!(
                        "{}/v1/users/{}/groups/?page_size=100&page_number={page_number}",
                        self.base_url,
                        path_segment(username)
                    ),
                    &[GROUP_FIELDS],
                )
            },
            |_| Ok(()),
        )
    }

    /// GET a FASJSON URL through curl (Kerberos rides on `--negotiate`)
    /// and parse the JSON.
    fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<T, Box<dyn std::error::Error>> {
        self.get_json_with(url, &[])
    }

    /// [`Self::get_json`], sending `headers` too.
    fn get_json_with<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        headers: &[&str],
    ) -> Result<T, Box<dyn std::error::Error>> {
        let output = Command::new(&self.curl[0])
            .args(&self.curl[1..])
            .args(curl_args(url, headers))
            .output()
            .map_err(|e| format!("failed to run curl: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("401") || stderr.contains("403") {
                return Err("Kerberos authentication failed — do you have a valid ticket?".into());
            }
            return Err(format!("curl failed (exit {}): {}", output.status, stderr.trim()).into());
        }

        serde_json::from_slice(&output.stdout)
            .map_err(|e| format!("failed to parse FASJSON response: {e}").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_members_page_parses_with_its_paging_block() {
        let json = r#"{"result": [{"username": "decathorpe"}, {"username": "zbyszek"}],
            "page": {"total_results": 10, "page_size": 2, "page_number": 1, "total_pages": 5,
                     "next_page": "https://fasjson.fedoraproject.org/v1/groups/rust-sig/members/?page_size=2&page_number=2"}}"#;
        let page: FasjsonPage<FasUser> = serde_json::from_str(json).unwrap();
        assert_eq!(page.result.len(), 2);
        assert_eq!(page.result[1].username, "zbyszek");
        let p = page.page.unwrap();
        assert_eq!((p.page_number, p.total_pages, p.total_results), (1, 5, 10));
    }

    fn page(names: &[&str], number: u32, total: u32) -> FasjsonPage<FasGroup> {
        serde_json::from_value(serde_json::json!({
            "result": names.iter().map(|n| serde_json::json!({"groupname": n})).collect::<Vec<_>>(),
            "page": {"total_results": 3, "page_size": 2, "page_number": number,
                     "total_pages": total}
        }))
        .unwrap()
    }

    /// A stand-in for curl that answers each URL (its last argument)
    /// with canned FASJSON: a script for sh, written to a fresh file under
    /// the temp dir.
    fn fake_curl(name: &str, script: &str) -> String {
        let dir =
            std::env::temp_dir().join(format!("sandogasa-fasjson-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("curl");
        std::fs::write(&path, format!("for url; do :; done\n{script}")).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn groups_and_a_users_groups_are_read_from_their_endpoints() {
        let curl = fake_curl(
            "groups",
            r#"case "$url" in
  */v1/groups/rust-sig/) echo '{"result": {"groupname": "rust-sig", "mailing_list": "rust@lists.fedoraproject.org"}}' ;;
  */v1/users/alice/groups/*page_number=1)
    case "$*" in
      *"X-Fields: groupname,description,mailing_list"*) list='"rust@lists.fedoraproject.org"' ;;
      *) list=null ;;
    esac
    echo "{\"result\": [{\"groupname\": \"rust-sig\", \"mailing_list\": $list}], \"page\": {\"total_results\": 2, \"page_size\": 1, \"page_number\": 1, \"total_pages\": 2}}" ;;
  */v1/users/alice/groups/*page_number=2) echo '{"result": [{"groupname": "packager"}], "page": {"total_results": 2, "page_size": 1, "page_number": 2, "total_pages": 2}}' ;;
  */v1/users/alice/) echo '{"result": {"username": "alice", "rhbzemail": "alice@example.org"}}' ;;
  */v1/groups/big/members/*) echo '{"result": [], "page": {"total_results": 500, "page_size": 100, "page_number": 1, "total_pages": 5}}' ;;
  *) echo "unexpected $url" >&2; exit 22 ;;
esac
"#,
        );
        let client = FasjsonClient::with_base_url("https://fasjson.example").with_curl(&curl);
        let group = client.group("rust-sig").unwrap();
        assert_eq!(
            group.mailing_list.as_deref(),
            Some("rust@lists.fedoraproject.org")
        );
        let groups = client.user_groups("alice").unwrap();
        let names: Vec<&str> = groups.iter().map(|g| g.groupname.as_str()).collect();
        assert_eq!(names, ["rust-sig", "packager"]);
        // FASJSON lists a user's groups by name alone unless the field
        // mask asks for the mailing list.
        assert_eq!(
            groups[0].mailing_list.as_deref(),
            Some("rust@lists.fedoraproject.org")
        );
        assert_eq!(
            client.user("alice").unwrap().rhbzemail.as_deref(),
            Some("alice@example.org")
        );
        let err = client.group_members("big", Some(10)).unwrap_err();
        assert!(err.to_string().contains("500 members"), "{err}");
        let err = client.group("other").unwrap_err();
        assert!(err.to_string().contains("curl failed"), "{err}");
    }

    #[test]
    fn a_refused_ticket_and_bad_json_are_said() {
        let refused = fake_curl(
            "refused",
            "echo 'The requested URL returned error: 401' >&2; exit 22\n",
        );
        let client = FasjsonClient::with_base_url("https://fasjson.example").with_curl(&refused);
        let err = client.group("rust-sig").unwrap_err();
        assert!(err.to_string().contains("Kerberos"), "{err}");
        let garbled = fake_curl("garbled", "echo 'not json'\n");
        let client = FasjsonClient::with_base_url("https://fasjson.example").with_curl(&garbled);
        let err = client.group("rust-sig").unwrap_err();
        assert!(err.to_string().contains("parse"), "{err}");
        let mut missing = FasjsonClient::with_base_url("https://fasjson.example");
        missing.curl = vec!["/nonexistent/curl".to_string()];
        let err = missing.group("rust-sig").unwrap_err();
        assert!(err.to_string().contains("failed to run curl"), "{err}");
    }

    #[test]
    fn a_name_stays_one_path_segment() {
        assert_eq!(path_segment("rust-sig"), "rust-sig");
        assert_eq!(path_segment(" go/../x"), "%20go%2F..%2Fx");
    }

    #[test]
    fn every_page_of_a_list_is_collected_in_order() {
        let mut asked = Vec::new();
        let groups = collect_pages(
            |n| {
                asked.push(n);
                Ok(match n {
                    1 => page(&["a", "b"], 1, 2),
                    _ => page(&["c"], 2, 2),
                })
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(asked, [1, 2]);
        let names: Vec<&str> = groups.iter().map(|g| g.groupname.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[test]
    fn a_refused_list_stops_after_its_first_page() {
        let mut asked = 0;
        let result = collect_pages(
            |_| {
                asked += 1;
                Ok(page(&["a", "b"], 1, 2))
            },
            |p| {
                if p.total_results > 2 {
                    Err("too many".into())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(result.unwrap_err().to_string(), "too many");
        assert_eq!(asked, 1);
    }

    #[test]
    fn a_list_without_paging_is_one_page() {
        let groups = collect_pages(
            |_| {
                Ok(serde_json::from_value::<FasjsonPage<FasGroup>>(
                    serde_json::json!({"result": [{"groupname": "a"}]}),
                )
                .unwrap())
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(groups.len(), 1);
    }

    #[test]
    fn curl_is_told_who_is_calling() {
        // Fedora's infrastructure tarpits requests without a user-agent,
        // and this crate is the one that shells out rather than using
        // reqwest, so nothing else would supply it.
        let args = curl_args("https://example.invalid/v1/users/alice/", &[GROUP_FIELDS]);
        let at = args
            .iter()
            .position(|a| *a == "--user-agent")
            .expect("no user-agent passed to curl");
        let sent = args[at + 1];
        assert!(sent.starts_with("sandogasa-fasjson/"), "{sent}");
        assert!(
            sent.len() > "sandogasa-fasjson/".len(),
            "no version in {sent}"
        );
        let at = args.iter().position(|a| *a == "-H").expect("no header");
        assert_eq!(args[at + 1], GROUP_FIELDS);
        // The URL stays last, behind `--`, so it can never be read as an
        // option however it is spelled.
        assert_eq!(args[args.len() - 2], "--");
        assert_eq!(
            *args.last().unwrap(),
            "https://example.invalid/v1/users/alice/"
        );
    }

    #[test]
    fn new_uses_default_base_url() {
        let client = FasjsonClient::new();
        assert_eq!(client.base_url, "https://fasjson.fedoraproject.org");
    }

    #[test]
    fn with_base_url_trims_trailing_slash() {
        let client = FasjsonClient::with_base_url("https://fasjson.example.com/");
        assert_eq!(client.base_url, "https://fasjson.example.com");
    }

    #[test]
    fn user_with_invalid_curl_returns_curl_error() {
        // Use a base_url that curl can't reach to test error path
        let client = FasjsonClient::with_base_url("http://127.0.0.1:1");
        let result = client.user("test");
        assert!(result.is_err());
    }
}
